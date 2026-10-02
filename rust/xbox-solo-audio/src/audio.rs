//! Reproducción: Opus (libopus, el crate `opus`) → `Reserva` (la política de
//! el prototipo en Node) → salida compartida de Windows con cpal
//! (WASAPI en modo compartido: Discord sigue sonando por los mismos auriculares).
//!
//! La tarjeta pide audio cuando quiere; cada 10 ms que consume se saca un bloque
//! de la reserva (`consumir_bloque`), que es quien decide estirar, recortar o
//! callar. Si la reserva no tiene bloque, suena silencio en ese hueco.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::registro::registrar;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::reserva::{Decodificador, Reserva};

const MUESTRAS_BLOQUE: usize = 480 * 2; // 10 ms estéreo a 48 kHz

/// Lo que tarda en sonar lo que se escribe en la tarjeta (`playback - callback`
/// de cpal: el audio ya encolado en WASAPI), en µs: el último y el máximo
/// desde la última lectura. Es la parte de la salida en la latencia del
/// cliente; no incluye la del propio dispositivo (USB, DAC).
pub static SALIDA_ULTIMA_US: AtomicU64 = AtomicU64::new(0);
pub static SALIDA_MAXIMA_US: AtomicU64 = AtomicU64::new(0);

/// CPU propia del camino de audio, en ns acumulados: toda la llamada de la
/// tarjeta (reserva + decodificación + copia) y, dentro, sólo libopus. Lo que
/// el hilo de cpal gaste además de esto es de cpal y WASAPI.
pub static LLAMADA_NS: AtomicU64 = AtomicU64::new(0);
pub static OPUS_NS: AtomicU64 = AtomicU64::new(0);

/// `--vigilar-volumen`: energía del audio descodificado de verdad
/// (sin PLC) para registrar el nivel RMS cada segundo.
/// sólo se calcula con la opción (era una pasada por decodificación).
pub static VIGILAR_VOLUMEN: AtomicBool = AtomicBool::new(false);
pub static VOLUMEN_SUMA: AtomicU64 = AtomicU64::new(0);
pub static VOLUMEN_MUESTRAS: AtomicU64 = AtomicU64::new(0);

/// libopus. `None` = PLC de 20 ms (paquete perdido o que no llega a tiempo).
pub struct Opus(opus::Decoder);

impl Opus {
    pub fn nuevo() -> Result<Self, opus::Error> { Ok(Self(opus::Decoder::new(48_000, opus::Channels::Stereo)?)) }
}

impl Decodificador for Opus {
    fn decodificar(&mut self, carga: Option<&[u8]>) -> Vec<i16> {
        let inicio = Instant::now();
        let mut pcm = vec![0i16; 960 * 2];
        // Con un paquete vacío libopus oculta tanto como se le pide: 960 muestras.
        let muestras = self.0.decode(carga.unwrap_or(&[]), &mut pcm, false).unwrap_or(0);
        pcm.truncate(muestras * 2);
        pcm.resize(960 * 2, 0);
        if carga.is_some() && VIGILAR_VOLUMEN.load(Ordering::Relaxed) {
            VOLUMEN_SUMA.fetch_add(pcm.iter().map(|&m| (i64::from(m) * i64::from(m)) as u64).sum(), Ordering::Relaxed);
            VOLUMEN_MUESTRAS.fetch_add(pcm.len() as u64, Ordering::Relaxed);
        }
        OPUS_NS.fetch_add(inicio.elapsed().as_nanos() as u64, Ordering::Relaxed);
        pcm
    }
}

pub type ReservaCompartida = Arc<Mutex<Reserva<Opus>>>;

/// Llena lo que pide la tarjeta (estéreo f32 intercalado) con bloques de 10 ms
/// de la reserva; lo comparten la salida de cpal y la de IAudioClient3.
/// Si la reserva se queda sin bloque (p. ej. el hueco de un relevo), no corta
/// en seco: repite el último bloque bajando el volumen en 10 ms hasta el
/// silencio, y al volver el audio sube en otros 10 ms (sin chasquidos).
#[derive(Default)]
pub struct Relleno { actual: Vec<i16>, posicion: usize, actual_es_audio: bool, ganancia: f32, objetivo: f32, elastico: crate::elastico::Elastico }

/// `--tapar-relevo`: la salida pasa por `elastico` (empalme y estiramiento).
pub static TAPAR: AtomicBool = AtomicBool::new(false);

/// Cada segundo, desde un hilo propio: cómo va la reserva de `--tapar-relevo`.
pub fn registrar_tapar(reserva: ReservaCompartida, t0: Instant) {
    let (mut antes, mut relevo_visto) = ((0u64, 0u64, 0u64, 0u64), 0u64);
    // Ritmo conseguido frente al pedido, por minuto.
    let (mut segundos, mut pedido_ms, mut estirado_antes) = (0u32, 0.0f64, 0u64);
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        let c = &crate::elastico::CIFRAS;
        let (objetivo, t, junta) = reserva.lock().map(|mut r| (r.objetivo_ms(), r.tapar, r.junta_lista.take())).unwrap_or((0, None, None));
        if let Some(j) = junta { registrar(t0, &format!("tapar: {j}")) }
        segundos += 1;
        pedido_ms += c.ritmo_ppm.load(Ordering::Relaxed) as f64 / 1e6 * 1000.0;
        if segundos == 60 {
            let estirado = c.insertadas.load(Ordering::Relaxed);
            registrar(t0, &format!("tapar: en el último minuto, estirados {} ms de {:.0} pedidos", (estirado - estirado_antes) / 48, pedido_ms));
            (segundos, pedido_ms, estirado_antes) = (0, 0.0, estirado);
        }
        // Lo que se tiró al principio de cada transporte nuevo: un poco de
        // silencio (50-300 ms) es la señal de que el Play no adelantó a Spotify.
        if let Some(t) = t {
            // Una línea por relevo, al acabar su ventana de 2,5 s.
            let ahora = (t.silencio_descartado, t.inicio_bajo, t.cola_recortada, t.pausa_tras_cabeza);
            if t.relevos > relevo_visto && t.empalme_hecho() {
                let quitado_nuevo = (ahora.0 - antes.0 + ahora.1 - antes.1 + ahora.3 - antes.3) * 20;
                registrar(t0, &format!("tapar: relevo {}: reserva antes del hueco ~{} ms, hueco {} ms, del nuevo quitados {} ms ({} de silencio, {} bajos antes de la música y {} de pausa tras la cabeza): déficit {} ms; cola del viejo recortada {} ms; números saltados al principio del nuevo {} en total",
                    t.relevos, t.nivel_al_llegar_ms + t.hueco_ms, t.hueco_ms, quitado_nuevo, (ahora.0 - antes.0) * 20, (ahora.1 - antes.1) * 20, (ahora.3 - antes.3) * 20,
                    t.hueco_ms + quitado_nuevo, (ahora.2 - antes.2) * 10, t.saltados));
                (antes, relevo_visto) = (ahora, t.relevos);
            }
        }
        registrar(t0, &format!("tapar: reserva {} ms (objetivo {objetivo}), ritmo {:.1} %, estirado {} ms, empalmes {} (sin cola {}), cola de pausa recortada {} ms, silencio descartado {} tras relevo, {} entre canciones y {} largo (>5 s)",
            c.nivel_ms.load(Ordering::Relaxed), c.ritmo_ppm.load(Ordering::Relaxed) as f64 / 1e4, c.insertadas.load(Ordering::Relaxed) / 48,
            c.empalmes.load(Ordering::Relaxed), c.sin_empalme.load(Ordering::Relaxed), t.map_or(0, |t| t.cola_recortada * 10),
            t.map_or(0, |t| t.silencio_descartado), t.map_or(0, |t| t.silencio_entre_canciones), t.map_or(0, |t| t.silencio_largo)));
    });
}

/// El ritmo de estiramiento, por tramos, 0 en los
/// últimos 40 ms. Un relevo cada ~56 s gasta 1,4-2,1 s (2,5-3,8 %).
///
/// 4 % mientras falten más de 150 ms y 2,5 % para los últimos 150.
/// Con el peor déficit medido (2,1 s de 3), 2,1 s → 2,85 s en 49 s y el resto
/// en 6: lleno a los ~55 s, antes del relevo siguiente (~56 s).
pub fn ritmo(nivel_ms: usize, objetivo_ms: usize) -> f64 {
    match objetivo_ms.saturating_sub(nivel_ms) { d if d > 150 => 0.04, d if d > 40 => 0.025, _ => 0.0 }
}

#[cfg(test)]
mod pruebas {
    #[test]
    fn volumen_con_limitador() {
        use super::con_volumen;
        assert_eq!(con_volumen(0.5, 1.0), 0.5);
        assert!((con_volumen(0.5, 1.2) - 0.6).abs() < 1e-6, "por debajo de la rodilla, ganancia lineal");
        for x in [0.7f32, 0.9, 1.0, -1.0] {
            let y = con_volumen(x, 1.5);
            assert!(y.abs() < 1.0, "nunca llega a 1: {x} → {y}");
            assert_eq!(y.signum(), x.signum());
        }
        assert!(con_volumen(0.9, 1.2) > con_volumen(0.8, 1.2), "monótona");
    }

    /// con 2,1 s de déficit sobre 3 s, la reserva se llena antes de 56 s.
    #[test]
    fn la_reserva_se_llena_antes_del_relevo_siguiente() {
        let (mut nivel, mut t) = (900.0f64, 0.0f64);
        while super::ritmo(nivel as usize, 3000) > 0.0 { nivel += super::ritmo(nivel as usize, 3000) * 10.0; t += 0.01; }
        assert!(t < 56.0, "{t} s");
    }

    /// lo que tarda la llamada de la tarjeta (10 ms de audio) con
    /// Opus de verdad y la reserva llena, y lo que pasa a decodificarse al llegar.
    /// `cargo test --release -- --ignored medir_llamada --nocapture`.
    #[test]
    #[ignore]
    fn medir_llamada() {
        use std::sync::{atomic::Ordering, Arc, Mutex};
        use std::time::{Duration, Instant};
        let mut cod = opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::Audio).unwrap();
        let tono: Vec<f32> = (0..960 * 2).map(|i| ((i / 2) as f32 * 0.05).sin() * 0.3).collect();
        let mut b = vec![0u8; 1500];
        let n = cod.encode_float(&tono, &mut b).unwrap();
        let paquete = b[..n].to_vec();
        let reserva: super::ReservaCompartida = Arc::new(Mutex::new(crate::reserva::Reserva::nueva(super::Opus::nuevo().unwrap(), 40, 40, 200, 60_000.0, 0.0)));
        let t0 = Instant::now();
        let mut relleno = super::Relleno::default();
        let mut datos = vec![0f32; super::MUESTRAS_BLOQUE];
        let llegar = |seq: u16| { reserva.lock().unwrap().recibir(seq, paquete.clone(), t0.elapsed().as_secs_f64() * 1000.0); };
        for seq in 0..3 { llegar(seq) }
        let (mut en_llamada, mut llamadas) = (Duration::ZERO, 0u32);
        let antes = super::OPUS_NS.load(Ordering::Relaxed);
        for seq in 3..2003u16 {
            for _ in 0..2 {
                let inicio = Instant::now();
                relleno.llenar(&mut datos, &reserva, t0);
                en_llamada += inicio.elapsed();
                llamadas += 1;
            }
            llegar(seq);
        }
        let opus_total = Duration::from_nanos(super::OPUS_NS.load(Ordering::Relaxed) - antes);
        let c = reserva.lock().unwrap().cuenta;
        println!("llamada de 10 ms: {:.2} µs de media ({llamadas} llamadas); libopus en total {:.2} µs por paquete; adelantados {} de {}",
            en_llamada.as_secs_f64() * 1e6 / f64::from(llamadas), opus_total.as_secs_f64() * 1e6 / 2003.0, c.adelantados, c.sonados);
    }

    /// sin `--salida`, se sigue a la predeterminada de Windows.
    #[test]
    fn cuando_reabrir_la_salida() {
        use super::reabrir_salida as r;
        let nunca = || -> bool { panic!("sin --salida no se enumeran dispositivos") };
        assert!(r(false, "", None, None, nunca));
        assert!(r(true, "", Some("Altavoces"), Some("Altavoces"), nunca));
        // El receptor vuelve: Windows lo hace predeterminado otra vez.
        assert!(r(false, "", Some("Altavoces"), Some("Auriculares (LIGHTSPEED)"), nunca));
        assert!(!r(false, "", Some("Altavoces"), Some("Altavoces"), nunca));
        assert!(!r(false, "", Some("Altavoces"), None, nunca));
        // Con --salida, lo de siempre.
        assert!(r(false, "lightspeed", Some("Altavoces"), None, || true));
        assert!(!r(false, "lightspeed", Some("Altavoces"), None, || false));
        assert!(!r(false, "lightspeed", Some("Auriculares (LIGHTSPEED)"), Some("Altavoces"), || true));
    }
}

impl Relleno {
    /// Con `--tapar-relevo`: 10 ms del elástico, que pide a la reserva lo que necesite.
    /// Sin registrar ni bloquear en el hilo de audio: las cifras van a
    /// `elastico::CIFRAS` y las registra `registrar_tapar` desde otro hilo.
    fn bloque_elastico(&mut self, reserva: &ReservaCompartida, ahora: f64, rafaga: bool) -> Option<Vec<i16>> {
        loop {
            if let Some(b) = self.elastico.sacar(crate::elastico::TRAMA) { return Some(b) }
            let (bloque, empalme, nivel, objetivo) = reserva.lock().ok().map(|mut r| {
                let b = r.consumir_bloque_de_llamada(ahora, rafaga).0;
                (b, r.empalme(), r.nivel_ms(), r.objetivo_ms() as usize)
            })?;
            let nivel = nivel + self.elastico.muestras_en_cola() / 48;
            self.elastico.ritmo = ritmo(nivel, objetivo);
            self.elastico.publicar(nivel);
            self.elastico.empujar(&bloque?, empalme);
        }
    }

    pub fn llenar(&mut self, datos: &mut [f32], reserva: &ReservaCompartida, t0: Instant) {
        const PASO: f32 = 1.0 / MUESTRAS_BLOQUE as f32;
        let mut primero = true;
        for muestra in datos.iter_mut() {
            if self.posicion >= self.actual.len() {
                // Toca otro bloque de 10 ms: lo decide la reserva. Los bloques
                // segundos de una misma llamada son ráfaga de la tarjeta, no de la red.
                let ahora = t0.elapsed().as_secs_f64() * 1000.0;
                let bloque = if TAPAR.load(Ordering::Relaxed) { self.bloque_elastico(reserva, ahora, !primero) } else {
                    reserva.lock().map(|mut r| r.consumir_bloque_de_llamada(ahora, !primero).0).unwrap_or(None)
                };
                primero = false;
                // Sin copias: el último bloque con audio se queda en
                // `actual` y se repite bajando el volumen; si ya no suena, ceros
                // en el mismo búfer.
                match bloque {
                    Some(b) => { self.objetivo = 1.0; self.actual = b; self.actual_es_audio = true }
                    None => {
                        self.objetivo = 0.0;
                        if !(self.ganancia > 0.0 && self.actual_es_audio) {
                            self.actual.clear();
                            self.actual.resize(MUESTRAS_BLOQUE, 0);
                            self.actual_es_audio = false;
                        }
                    }
                }
                self.posicion = 0;
            }
            if self.ganancia < self.objetivo { self.ganancia = (self.ganancia + PASO).min(self.objetivo) }
            else if self.ganancia > self.objetivo { self.ganancia = (self.ganancia - PASO).max(self.objetivo) }
            *muestra = self.actual[self.posicion] as f32 / 32768.0 * self.ganancia;
            self.posicion += 1;
        }
        let v = f32::from_bits(VOLUMEN.load(Ordering::Relaxed));
        if v != 1.0 { for m in datos.iter_mut() { *m = con_volumen(*m, v) } }
        crate::volcado::escribir(datos);
    }
}

/// `--volumen=N`: la
/// ganancia de la salida, en bits de `f32` (1.0 = como llega). Por encima de 1 un
/// limitador suave (`con_volumen`) evita que los picos recorten.
pub static VOLUMEN: AtomicU32 = AtomicU32::new(0x3f80_0000);

pub fn fijar_volumen(porcentaje: f32) { VOLUMEN.store((porcentaje.clamp(0.0, 400.0) / 100.0).to_bits(), Ordering::Relaxed) }

/// Umbral del limitador: por debajo pasa igual; por encima se comprime con tanh
/// hacia 1,0 sin llegar nunca (sin recorte duro ni estado: no añade latencia).
const RODILLA: f32 = 0.8;

/// Una muestra con la ganancia `v` aplicada y el limitador suave.
pub fn con_volumen(x: f32, v: f32) -> f32 {
    let y = x * v;
    let a = y.abs();
    if a <= RODILLA { y } else { y.signum() * (RODILLA + (1.0 - RODILLA) * ((a - RODILLA) / (1.0 - RODILLA)).tanh()) }
}

/// `--salida-iac3`: IAudioClient3 (sólo Windows) en vez de cpal.
pub static SALIDA_IAC3: AtomicBool = AtomicBool::new(false);

/// Lo que mantiene viva una salida abierta.
enum Salida {
    Cpal(#[allow(dead_code)] cpal::Stream),
    #[cfg(windows)]
    Iac3(#[allow(dead_code)] crate::iac3::Iac3),
}

fn coincide(nombre: &str, salida: &str) -> bool { nombre.to_lowercase().contains(&salida.to_lowercase()) }

/// ¿Hay que reabrir la salida? `actual`: el nombre de la abierta (None: ninguna).
/// Con `--salida`, cuando la pedida vuelve a estar y la abierta es otra. Sin
/// `--salida` (los dos accesos), cuando la predeterminada de Windows es otra: al
/// volver el receptor USB, Windows la hace predeterminada otra vez y antes el
/// exe se quedaba en los altavoces. `pedida_esta` se consulta sólo
/// si hace falta (enumerar dispositivos cuesta).
pub fn reabrir_salida(rota: bool, salida: &str, actual: Option<&str>, predeterminada: Option<&str>, pedida_esta: impl FnOnce() -> bool) -> bool {
    let Some(actual) = actual else { return true };
    if rota { return true }
    if salida.is_empty() { return predeterminada.is_some_and(|p| p != actual) }
    !coincide(actual, salida) && pedida_esta()
}

/// Mantiene la salida abierta en su propio hilo (el flujo de cpal no se puede
/// mover entre hilos). Si falla —se desenchufa el receptor LIGHTSPEED— se
/// reabre con la predeterminada de Windows, y cuando la preferida vuelve se
/// pasa a ella, como el prototipo en Node. Sana, sólo mira una bandera.
pub fn vigilar_salida(salida: String, reserva: ReservaCompartida, t0: Instant) {
    std::thread::spawn(move || {
        let rota = Arc::new(AtomicBool::new(false));
        let mut actual: Option<(Salida, String)> = None;
        loop {
            let predeterminada = if salida.is_empty() && actual.is_some() { cpal::default_host().default_output_device().and_then(|d| d.name().ok()) } else { None };
            let reabrir = reabrir_salida(rota.load(Ordering::Relaxed), &salida, actual.as_ref().map(|(_, n)| n.as_str()), predeterminada.as_deref(),
                || cpal::default_host().output_devices().map(|mut d| d.any(|d| d.name().map(|n| coincide(&n, &salida)).unwrap_or(false))).unwrap_or(false));
            if reabrir {
                if let (Some((_, antes)), Some(ahora)) = (&actual, &predeterminada) { registrar(t0, &format!("audio: la predeterminada de Windows pasa de «{antes}» a «{ahora}»: se reabre")) }
            }
            if reabrir {
                actual = None;
                rota.store(false, Ordering::Relaxed);
                #[cfg(windows)]
                let abierta = if SALIDA_IAC3.load(Ordering::Relaxed) {
                    crate::iac3::abrir(&salida, reserva.clone(), t0, rota.clone()).map(|(s, n)| (Salida::Iac3(s), n))
                } else { abrir_salida(&salida, reserva.clone(), t0, rota.clone()).map(|(s, n)| (Salida::Cpal(s), n)) };
                #[cfg(not(windows))]
                let abierta = abrir_salida(&salida, reserva.clone(), t0, rota.clone()).map(|(s, n)| (Salida::Cpal(s), n));
                match abierta {
                    Ok((flujo, nombre)) => {
                        let provisional = !salida.is_empty() && !coincide(&nombre, &salida);
                        registrar(t0, &format!("audio: {nombre}, salida compartida{}", if provisional { " (predeterminada provisional)" } else { "" }));
                        actual = Some((flujo, nombre));
                    }
                    Err(e) => registrar(t0, &format!("audio: apertura pendiente: {e}")),
                }
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    });
}

/// Abre la salida (`salida` = parte del nombre; vacío = la predeterminada) y
/// devuelve el flujo, que hay que mantener vivo mientras suene.
pub fn abrir_salida(salida: &str, reserva: ReservaCompartida, t0: Instant, rota: Arc<AtomicBool>) -> Result<(cpal::Stream, String), Box<dyn std::error::Error + Send + Sync>> {
    let host = cpal::default_host();
    let dispositivo = if salida.is_empty() {
        host.default_output_device()
    } else {
        host.output_devices()?.find(|d| d.name().map(|n| n.to_lowercase().contains(&salida.to_lowercase())).unwrap_or(false))
            .or_else(|| host.default_output_device())
    }.ok_or("no hay salida de audio")?;
    let nombre = dispositivo.name().unwrap_or_default();
    // Opus decodifica a 48 kHz: si la salida está a otra frecuencia habría que
    // remuestrear, y eso es calidad perdida. Los G321 están a 48 kHz (medido).
    let config = cpal::StreamConfig { channels: 2, sample_rate: cpal::SampleRate(48_000), buffer_size: cpal::BufferSize::Default };

    let mut relleno = Relleno::default();
    // Diagnóstico: cuánto pide la tarjeta en cada llamada (el periodo de WASAPI).
    let mut pedidos_vistos: Vec<usize> = Vec::new();
    let flujo = dispositivo.build_output_stream(
        &config,
        move |datos: &mut [f32], info: &cpal::OutputCallbackInfo| {
            let inicio = Instant::now();
            let marca = info.timestamp();
            if let Some(espera) = marca.playback.duration_since(&marca.callback) {
                let us = espera.as_micros() as u64;
                SALIDA_ULTIMA_US.store(us, Ordering::Relaxed);
                SALIDA_MAXIMA_US.fetch_max(us, Ordering::Relaxed);
            }
            let pedido = datos.len() / 2;
            if pedidos_vistos.len() < 8 && !pedidos_vistos.contains(&pedido) {
                pedidos_vistos.push(pedido);
                registrar(t0, &format!("la tarjeta pide {pedido} muestras ({:.1} ms) por llamada", pedido as f64 / 48.0));
            }
            relleno.llenar(datos, &reserva, t0);
            LLAMADA_NS.fetch_add(inicio.elapsed().as_nanos() as u64, Ordering::Relaxed);
        },
        move |error| {
            registrar(t0, &format!("audio: error de salida: {error}"));
            rota.store(true, Ordering::Relaxed);
        },
        None,
    )?;
    flujo.play()?;
    Ok((flujo, nombre))
}
