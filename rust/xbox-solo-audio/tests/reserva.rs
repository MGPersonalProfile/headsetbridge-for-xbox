use xbox_solo_audio::reserva::{es_silencio, Decodificador, Emision, Reserva, TipoEmision};

struct DecodificadorFalso;
impl Decodificador for DecodificadorFalso {
    fn decodificar(&mut self, carga: Option<&[u8]>) -> Vec<i16> {
        let byte = carga.and_then(|c| c.first()).copied().unwrap_or(255);
        vec![i16::from_le_bytes([byte, byte]); 960 * 2]
    }
}

struct Banco {
    reserva: Reserva<DecodificadorFalso>,
    ahora_ms: f64,
    pcm: Vec<u8>,
    tipos: Vec<TipoEmision>,
}

impl Banco {
    fn nuevo(reserva_ms: u32) -> Self {
        Self { reserva: Reserva::nueva(DecodificadorFalso, reserva_ms, 20, 80, 60_000.0, 0.0),
               ahora_ms: 0.0, pcm: vec![], tipos: vec![] }
    }
    fn recoger(&mut self, emisiones: Vec<Emision>) {
        for emision in emisiones {
            self.pcm.push(emision.primera_muestra as u8);
            self.tipos.push(emision.tipo);
        }
    }
    fn recibir(&mut self, seq: u16, retraso_ms: f64) {
        self.ahora_ms = f64::from(seq) * 20.0 + retraso_ms;
        let emisiones = self.reserva.recibir(seq, vec![seq as u8], self.ahora_ms);
        self.recoger(emisiones);
    }
    fn sonar(&mut self) {
        assert!(self.reserva.bloques_pendientes() > 0, "había PCM pendiente en la tarjeta");
        let (bloque, emisiones) = self.reserva.consumir_bloque(self.ahora_ms);
        assert_eq!(bloque.unwrap().len(), 480 * 2);
        self.recoger(emisiones);
    }
}

#[test]
fn orden_normal_duplicado_y_espera_de_reserva() {
    let mut b = Banco::nuevo(40);
    b.recibir(1, 0.0);
    assert!(b.pcm.is_empty());
    b.recibir(2, 0.0);
    assert_eq!(b.pcm, [1, 2]);
    b.recibir(2, 0.0);
    assert_eq!(b.reserva.cuenta.sonados, 2);
    assert_eq!(b.reserva.cuenta.duplicados, 1);
    assert_eq!(b.reserva.bloques_pendientes(), 4);
}

#[test]
fn reordenado_llega_antes_del_plazo_de_tarjeta() {
    let mut b = Banco::nuevo(40);
    b.recibir(10, 0.0); b.recibir(11, 0.0); b.recibir(13, 0.0);
    assert_eq!(b.reserva.cuenta.ocultados, 0);
    b.sonar();
    b.recibir(12, 0.0);
    b.sonar(); b.sonar();
    assert_eq!(b.pcm, [10, 11, 12, 13]);
    assert_eq!(b.reserva.cuenta.ocultados, 0);
}

#[test]
fn perdida_vence_una_vez_y_paquete_posterior_es_tardio() {
    let mut b = Banco::nuevo(40);
    b.recibir(20, 0.0); b.recibir(21, 0.0); b.recibir(23, 0.0);
    b.sonar(); b.sonar(); b.sonar();
    assert_eq!(b.pcm, [20, 21]);
    b.sonar();
    assert_eq!(b.pcm, [20, 21, 255, 23]);
    b.recibir(22, 0.0);
    assert_eq!(b.reserva.cuenta.ocultados, 1);
    assert_eq!(b.reserva.cuenta.tarde, 1);
    assert_eq!(b.reserva.cuenta.sonados, 3);
}

#[test]
fn saltos_grandes_y_reinicio_hacia_atras_resincronizan_sin_plc() {
    let mut b = Banco::nuevo(40);
    b.recibir(30, 0.0); b.recibir(31, 0.0);
    b.recibir(100, 0.0); b.recibir(101, 0.0); b.sonar();
    assert!(b.pcm.contains(&100));
    assert_eq!(b.reserva.cuenta.ocultados, 0);

    let mut b = Banco::nuevo(40);
    b.recibir(10000, 0.0); b.recibir(10001, 0.0);
    b.recibir(1, 0.0); b.recibir(2, 0.0);
    b.sonar(); b.sonar(); b.sonar();
    assert_eq!(b.reserva.siguiente, Some(3));
    assert_eq!(b.reserva.cuenta.ocultados, 0);
}

#[test]
fn corte_real_tras_cien_ms_de_plc_reune_de_nuevo_la_reserva() {
    let mut b = Banco::nuevo(20);
    b.recibir(1, 0.0);
    for _ in 0..40 {
        if b.reserva.cuenta.vaciados != 0 { break; }
        b.sonar();
    }
    assert_eq!(b.reserva.cuenta.ocultados, 5);
    assert_eq!(b.reserva.cuenta.retrasos, 1);
    assert_eq!(b.reserva.cuenta.vaciados, 1);
    assert!(!b.reserva.sonando);
    b.recibir(7, 0.0);
    assert_eq!(b.pcm.len(), 6);
    b.recibir(8, 0.0);
    assert_eq!(b.pcm, [1, 255, 255, 255, 255, 255, 7, 8]);
}

#[test]
fn plc_estira_y_dos_retrasos_en_diez_segundos_suben_diez_ms() {
    let mut b = Banco::nuevo(20);
    b.recibir(1, 0.0); b.recibir(2, 0.0);
    while b.reserva.cuenta.ocultados == 0 { b.sonar(); }
    assert_eq!(b.reserva.cuenta.estirados, 1);
    assert_eq!(b.reserva.cuenta.retrasos, 1);
    assert_eq!(b.reserva.objetivo_ms(), 20);
    b.recibir(3, 30.0);
    assert_eq!(b.reserva.cuenta.tarde, 0);
    while !b.pcm.contains(&3) { b.sonar(); }
    assert_eq!(b.pcm, [1, 2, 255, 3]);
    while b.reserva.cuenta.ocultados < 2 { b.sonar(); }
    assert_eq!(b.reserva.cuenta.retrasos, 2);
    assert_eq!(b.reserva.cuenta.vaciados, 0);
    assert_eq!(b.reserva.objetivo_ms(), 30);
}

#[test]
fn cola_sobrante_recorta_silencio_pero_conserva_sonido() {
    let mut b = Banco::nuevo(20);
    for seq in 252..=258 { b.recibir(seq, f64::from(258 - seq) * -20.0); }
    for _ in 0..20 {
        if b.reserva.bloques_pendientes() > 0 { b.sonar(); }
    }
    assert!(b.reserva.cuenta.recortados >= 1);
    assert!(!b.pcm.contains(&0));
    for marca in [252, 253, 254, 1, 2] { assert!(b.pcm.contains(&marca), "falta {marca}"); }
}

#[test]
fn variacion_de_llegada_se_mide_sin_inflar_objetivo() {
    let mut b = Banco::nuevo(20);
    for seq in 1..=5 { b.recibir(seq, 0.0); }
    b.recibir(6, 35.0);
    assert_eq!(b.reserva.retraso_de_red_ms(), 35);
    assert_eq!(b.reserva.objetivo_ms(), 20);
}

#[test]
fn silencio_respeta_el_umbral_int16_de_node() {
    assert!(es_silencio(&[-32, 0, 32]));
    assert!(!es_silencio(&[-33, 0]));
    assert!(!es_silencio(&[33]));
}

#[test]
fn una_rafaga_de_la_tarjeta_estira_sin_contar_como_retraso_de_red() {
    // La tarjeta pide 20-22 ms de golpe cuando Windows llega tarde (medido con cpal):
    // vaciar la reserva así no es culpa de la red y no debe subir el objetivo.
    let mut b = Banco::nuevo(20);
    b.recibir(1, 0.0);
    b.recibir(2, 0.0);
    for _ in 0..6 {
        let (_, emisiones) = b.reserva.consumir_bloque_de_llamada(b.ahora_ms, true);
        b.recoger(emisiones);
    }
    assert!(b.reserva.cuenta.estirados >= 1, "se estira: no hay hueco de silencio");
    assert_eq!(b.reserva.cuenta.retrasos, 0);
    assert_eq!(b.reserva.objetivo_ms(), 20);
}
