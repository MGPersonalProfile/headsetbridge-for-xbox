//! Política pura del receptor de el prototipo en Node.
//! El decodificador y la tarjeta se inyectan: este módulo no abre WASAPI.

use std::collections::{HashMap, VecDeque};

const DISTANCIA_MAXIMA: u16 = 50;
const MAX_OCULTOS_SEGUIDOS: u8 = 5;
const REPETICION_MS: f64 = 10_000.0;
const TRAMO_MS: f64 = 5_000.0;
const VENTANA_RED_MS: f64 = 60_000.0;
const MUESTRAS_BLOQUE: usize = 480 * 2; // 10 ms, estéreo a 48 kHz.

pub trait Decodificador {
    /// `None` solicita PLC de 20 ms; la salida son 960 muestras por canal.
    fn decodificar(&mut self, carga: Option<&[u8]>) -> Vec<i16>;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Cuenta {
    pub sonados: u64,
    pub ocultados: u64,
    pub estirados: u64,
    pub recortados: u64,
    pub retrasos: u64,
    pub tarde: u64,
    pub duplicados: u64,
    pub vaciados: u64,
    /// paquetes decodificados al llegar (en el hilo de red) en vez
    /// de en la llamada de la tarjeta.
    pub adelantados: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TipoEmision {
    Real(u16),
    PlcPerdida(u16),
    PlcEstirado(u16),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Emision {
    pub tipo: TipoEmision,
    pub primera_muestra: i16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Resuelto { Real, Oculto }

#[derive(Clone, Copy, Debug)]
struct Tramo {
    inicio: f64,
    t0: f64,
    seq0: u16,
    min: f64,
    max: f64,
}

pub struct Reserva<D: Decodificador> {
    decodificador: D,
    /// el PCM del paquete `siguiente`, decodificado al llegar.
    predecodificado: Option<(u16, Vec<i16>)>,
    paquetes: HashMap<u16, Vec<u8>>,
    resueltos: HashMap<u16, Resuelto>,
    orden_resueltos: VecDeque<u16>,
    tramos: VecDeque<Tramo>,
    bloques: VecDeque<Vec<i16>>,
    pub siguiente: Option<u16>,
    pub sonando: bool,
    pub cuenta: Cuenta,
    en_rafaga: bool,
    reserva_ms: u32,
    minima_ms: u32,
    maxima_ms: u32,
    calma_ms: f64,
    ocultos_seguidos: u8,
    ultimo_retraso_ms: f64,
    ultimo_vaciado_ms: f64,
    /// `--tapar-relevo`: reserva de música que tapa el hueco del relevo.
    pub tapar: Option<Tapar>,
    /// de dónde salió cada bloque de `bloques` (R real, P PLC por
    /// perdido, E PLC estirado por falta, V del viejo al reanclar, v PLC del viejo al reanclar).
    origenes: VecDeque<u8>,
    /// La junta del último relevo: los últimos 15 bloques del viejo y los
    /// primeros 15 del nuevo, con su nivel y origen, para el registro.
    junta: Option<(String, Vec<String>)>,
    pub junta_lista: Option<String>,
    /// los puntos de empalme pendientes (índice absoluto del primer
    /// bloque tras cada corte), los bloques metidos y sacados, y si el último
    /// sacado era uno de esos.
    marcas: VecDeque<u64>,
    empujados: u64,
    sacados: u64,
    empalme_ultimo: bool,
}

/// Lo que hace distinta la reserva con `--tapar-relevo`.
#[derive(Default, Clone, Copy, Debug)]
pub struct Tapar {
    /// Bloques viejos que quedan antes del primero del transporte nuevo.
    /// la fase de tras un relevo. 0: antes de la primera música
    /// del nuevo (se quita la pausa y el silencio); 1: en la cabeza de música
    /// (20-50 ms que llegan antes de que Spotify se pare); 2: quitando la pausa
    /// que sigue a la cabeza; 3: hecho.
    fase: u8,
    /// Paquetes de música de la cabeza.
    cabeza: u32,
    quitados_ventana: u32,
    /// Relevos vistos (para la línea de registro de cada uno).
    pub relevos: u64,
    /// Del último relevo: el hueco sin audio y la reserva al llegar lo nuevo (ms).
    pub hueco_ms: u64,
    pub nivel_al_llegar_ms: u64,
    /// Paquetes de silencio seguidos: entre canciones se deja medio segundo
    /// (25) y sólo se descarta lo que pase de ahí (no comerse
    /// colas de reverberación, fundidos ni pianísimos).
    silencio_seguido: u32,
    pub silencio_descartado: u64,
    pub silencio_entre_canciones: u64,
    /// El silencio de más de 5 s seguidos (Spotify en pausa), aparte.
    pub silencio_largo: u64,
    /// Lo recortado de la cola del transporte viejo (la pausa de Spotify), en bloques.
    pub cola_recortada: u64,
    /// Nivel de la música del transporte viejo (dB), medido al reanclar.
    referencia_db: Option<f64>,
    /// Paquetes del principio del transporte nuevo descartados por estar ≥ 20 dB
    /// por debajo de la música de antes (la cola de la pausa de Spotify, que
    /// llega con el transporte nuevo).
    pub inicio_bajo: u64,
    /// Números de paquete que faltaban al principio de un transporte nuevo y se saltaron.
    pub saltados: u64,
    /// Paquetes que quedan de los 3 s de tras el relevo (la limpieza no pasa de ahí).
    principio: u32,
    /// la pausa que llega tras la cabeza de música, quitada (paquetes).
    pub pausa_tras_cabeza: u64,
}

impl Tapar {
    /// Pasó ya lo de tras el relevo.
    pub fn empalme_hecho(&self) -> bool { self.fase == 3 || self.principio == 0 }

    /// lo nuevo está en la pausa de Spotify (tras la cabeza, o sin
    /// cabeza y ya 300 ms de pausa): el Play aún no ha hecho efecto. Se ve al
    /// llegar el paquete, no al sonar: hay toda la reserva de margen.
    pub fn en_pausa(&self) -> bool { self.pausa() == Pausa::EnPausa }

    /// dónde está lo nuevo tras el relevo.
    /// `Musica` sólo con música de verdad (la reanudación tras la pausa, o una
    /// cabeza de más de 25 paquetes); `Fuera` cuando la ventana de 3 s se acaba
    /// sin ella (antes eso parecía «volvió la música»); y la cabeza de un
    /// paquete entre dos tramos de pausa es `Esperando`, no la vuelta.
    pub fn pausa(&self) -> Pausa {
        if self.fase == 3 { Pausa::Musica }
        else if self.principio == 0 { Pausa::Fuera }
        else if self.fase == 2 || (self.fase == 0 && self.quitados_ventana >= 15) { Pausa::EnPausa }
        else { Pausa::Esperando }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pausa { Fuera, Esperando, EnPausa, Musica }

/// Nivel RMS de un bloque, en dB respecto al fondo de escala.
fn db(bloque: &[i16]) -> f64 {
    let e = bloque.iter().map(|&x| f64::from(x).powi(2)).sum::<f64>() / bloque.len().max(1) as f64;
    10.0 * (e / (32768.0 * 32768.0)).max(1e-12).log10()
}

/// ¿suena a la pausa de Spotify? Por debajo de −45 dBFS y ≥ 20 dB
/// por debajo de la música de antes (si se sabe).
fn es_pausa(pcm: &[i16], referencia_db: Option<f64>) -> bool {
    let d = db(pcm);
    d < -45.0 && referencia_db.is_none_or(|r| d < r - 20.0)
}

/// el nivel de la música de antes de un relevo: el percentil 75
/// de los últimos 2,5 s (sin los bloques mudos), robusto aunque la cola de la
/// pausa sea larga (600 ms a −80 dBFS, medido).
fn nivel_de_referencia(bloques: &VecDeque<Vec<i16>>) -> Option<f64> {
    let mut n: Vec<f64> = bloques.iter().rev().take(250).map(|b| db(b)).filter(|d| *d > -100.0).collect();
    if n.len() < 10 { return None }
    n.sort_by(|a, b| a.total_cmp(b));
    Some(n[n.len() * 3 / 4])
}

impl<D: Decodificador> Reserva<D> {
    pub fn nueva(decodificador: D, reserva_ms: u32, minima_ms: u32, maxima_ms: u32,
                 calma_ms: f64, ahora_ms: f64) -> Self {
        Self {
            decodificador, predecodificado: None, paquetes: HashMap::new(), resueltos: HashMap::new(),
            orden_resueltos: VecDeque::new(), tramos: VecDeque::new(),
            bloques: VecDeque::new(), siguiente: None, sonando: false,
            cuenta: Cuenta::default(), reserva_ms, minima_ms, maxima_ms, calma_ms,
            ocultos_seguidos: 0, ultimo_retraso_ms: f64::NEG_INFINITY,
            ultimo_vaciado_ms: ahora_ms, en_rafaga: false, tapar: None, origenes: VecDeque::new(), junta: None, junta_lista: None, marcas: VecDeque::new(), empujados: 0, sacados: 0, empalme_ultimo: false,
        }
    }

    /// Cambia reserva y mínimo sobre la marcha (`--reserva-inicial`): lo que
    /// sobre se recorta con el silencio, como siempre.
    pub fn ajustar(&mut self, reserva_ms: u32, minima_ms: u32) {
        self.reserva_ms = reserva_ms;
        self.minima_ms = minima_ms;
    }

    /// Otra fuente con otra numeración (el relevo de sesiones): se olvida la
    /// secuencia, pero no los bloques ya decodificados, que siguen sonando.
    pub fn reanclar(&mut self) { self.reanclar_tras(0) }

    /// `reanclar` en un relevo, con el hueco sin audio que hubo (para el registro).
    pub fn reanclar_tras(&mut self, hueco_ms: u64) {
        let nivel = self.cola_ms() as u64;
        if let Some(t) = &mut self.tapar {
            t.hueco_ms = hueco_ms;
            t.nivel_al_llegar_ms = nivel;
            t.relevos += 1;
            t.fase = 0;
            t.cabeza = 0;
            t.principio = 150;
            t.quitados_ventana = 0;
        }
        if self.tapar.is_some() { self.recortar_cola_vieja() }
        self.predecodificado = None;
        self.paquetes.clear();
        self.resueltos.clear();
        self.orden_resueltos.clear();
        self.tramos.clear();
        self.siguiente = None;
    }

    /// Lo más lejos de `siguiente` que puede llegar un paquete sin que se tome por
    /// un salto de secuencia: 50 paquetes, más los que caben en el objetivo
    /// (con 2 s de reserva, 50 fijos la vaciaban sin parar).
    fn distancia_maxima(&self) -> u16 { DISTANCIA_MAXIMA.saturating_add((self.objetivo_ms() / 20).min(1000) as u16) }

    /// Lo que hay en la reserva: decodificado y por decodificar (ms).
    pub fn nivel_ms(&self) -> usize { self.cola_ms() }

    pub fn objetivo_ms(&self) -> u32 {
        self.reserva_ms.clamp(self.minima_ms, self.maxima_ms)
    }

    pub fn bloques_pendientes(&self) -> usize { self.bloques.len() }

    /// paquetes esperando, bloques decodificados y
    /// resueltos recordados, y lo reservado para ellos (capacidad).
    pub fn tamanos(&self) -> String {
        format!("paquetes {} (cap. {}), bloques {} (cap. {}), resueltos {}", self.paquetes.len(), self.paquetes.capacity(), self.bloques.len(), self.bloques.capacity(), self.resueltos.len())
    }

    pub fn retraso_de_red_ms(&self) -> u32 {
        self.tramos.iter().map(|t| t.max - t.min).fold(0.0, f64::max).round() as u32
    }

    fn recordar(&mut self, seq: u16, tipo: Resuelto) {
        if !self.resueltos.contains_key(&seq) { self.orden_resueltos.push_back(seq); }
        self.resueltos.insert(seq, tipo);
        if self.resueltos.len() > 512 {
            if let Some(viejo) = self.orden_resueltos.pop_front() { self.resueltos.remove(&viejo); }
        }
    }

    fn medir_llegada(&mut self, seq: u16, ahora_ms: f64) {
        if self.tramos.back().is_none_or(|t| ahora_ms - t.inicio > TRAMO_MS) {
            self.tramos.push_back(Tramo { inicio: ahora_ms, t0: ahora_ms, seq0: seq, min: 0.0, max: 0.0 });
            while self.tramos.len() > 1 && ahora_ms - self.tramos.front().unwrap().inicio > VENTANA_RED_MS {
                self.tramos.pop_front();
            }
            return;
        }
        let tramo = self.tramos.back_mut().unwrap();
        let distancia = seq.wrapping_sub(tramo.seq0) as i32;
        let distancia = if distancia > 32768 { distancia - 65536 } else { distancia };
        let retraso = ahora_ms - (tramo.t0 + f64::from(distancia) * 20.0);
        tramo.min = tramo.min.min(retraso);
        tramo.max = tramo.max.max(retraso);
    }

    /// `ahora_ms` se inyecta para verificar la misma ventana de 10 s que Node.
    pub fn recibir(&mut self, seq: u16, carga: Vec<u8>, ahora_ms: f64) -> Vec<Emision> {
        self.medir_llegada(seq, ahora_ms);
        if self.siguiente.is_none() {
            self.siguiente = Some(seq);
            self.ultimo_vaciado_ms = ahora_ms;
        }
        let distancia = seq.wrapping_sub(self.siguiente.unwrap());
        if distancia > 32768 {
            match self.resueltos.get(&seq) {
                Some(Resuelto::Real) => { self.cuenta.duplicados += 1; return vec![]; }
                Some(Resuelto::Oculto) => { self.cuenta.tarde += 1; return vec![]; }
                None if (0u16).wrapping_sub(distancia) <= self.distancia_maxima() => {
                    self.cuenta.tarde += 1; return vec![];
                }
                None => {}
            }
        }
        if self.paquetes.contains_key(&seq) { self.cuenta.duplicados += 1; return vec![]; }
        if distancia > self.distancia_maxima() {
            // Un salto grande, también hacia atrás, reinicia la secuencia sin una ristra de PLC.
            self.predecodificado = None;
            self.paquetes.clear();
            self.resueltos.clear();
            self.orden_resueltos.clear();
            self.tramos.clear();
            self.siguiente = Some(seq);
        }
        self.paquetes.insert(seq, carga);
        let emitidos = self.rellenar(ahora_ms);
        self.predecodificar();
        emitidos
    }

    /// libopus fuera de la llamada de la tarjeta. Con la reserva
    /// llena, el paquete que llega espera a que la tarjeta saque un bloque y se
    /// decodificaba allí. Ahora el `siguiente`, si ya está, se decodifica al
    /// llegar (hilo de red) y `rellenar` lo usa tal cual: el mismo paquete, en
    /// el mismo orden para libopus (antes de él no falta nada, así que no hay
    /// PLC entre medias); qué se hace con él (reserva, tapado) no cambia.
    fn predecodificar(&mut self) {
        if self.predecodificado.is_some() { return }
        let Some(seq) = self.siguiente else { return };
        let Some(carga) = self.paquetes.get(&seq) else { return };
        let pcm = self.decodificador.decodificar(Some(carga));
        self.predecodificado = Some((seq, pcm));
        self.cuenta.adelantados += 1;
    }

    /// El PCM de `seq`: el ya decodificado al llegar o, si no, ahora.
    fn decodificar(&mut self, seq: u16, carga: &[u8]) -> Vec<i16> {
        match self.predecodificado.take() {
            Some((s, pcm)) if s == seq => pcm,
            _ => self.decodificador.decodificar(Some(carga)),
        }
    }

    /// Simula el callback de la tarjeta que consumió un bloque de 10 ms.
    pub fn consumir_bloque(&mut self, ahora_ms: f64) -> (Option<Vec<i16>>, Vec<Emision>) {
        self.consumir_bloque_de_llamada(ahora_ms, false)
    }

    /// Como `consumir_bloque`, para la tarjeta que pide varios bloques en una
    /// llamada (WASAPI por cpal pide a veces 20-22 ms de golpe cuando Windows
    /// llega tarde, medido). `rafaga` = no es el primer bloque de la llamada: si
    /// falta audio ahí, la culpa es de la tarjeta y no de la red, así que se
    /// estira pero no cuenta como retraso (no sube la reserva).
    pub fn consumir_bloque_de_llamada(&mut self, ahora_ms: f64, rafaga: bool) -> (Option<Vec<i16>>, Vec<Emision>) {
        self.en_rafaga = rafaga;
        self.empalme_ultimo = false;
        let bloque = self.bloques.pop_front();
        self.origenes.pop_front();
        if bloque.is_some() {
            while self.marcas.front().is_some_and(|m| *m < self.sacados) { self.marcas.pop_front(); }
            if self.marcas.front() == Some(&self.sacados) { self.marcas.pop_front(); self.empalme_ultimo = true }
            self.sacados += 1;
        }
        let emitidos = self.rellenar(ahora_ms);
        self.en_rafaga = false;
        (bloque, emitidos)
    }

    /// al llegar el transporte nuevo: lo que quedaba del viejo sin
    /// decodificar (tras un paquete perdido, con la reserva grande se espera)
    /// se decodifica, con PLC en los huecos, en vez de tirarlo; y su cola baja
    /// (la pausa de Spotify: lo que acaba ≥ 20 dB por debajo de su música, hasta
    /// 2,5 s) se quita. El empalme va desde ahí.
    fn recortar_cola_vieja(&mut self) {
        if let Some(mut seq) = self.siguiente {
            let ultimo = self.paquetes.keys().map(|&s| s.wrapping_sub(seq)).filter(|d| *d < 32768).max();
            if let Some(ultimo) = ultimo {
                for _ in 0..=ultimo.min(150) {
                    let (pcm, o) = match self.paquetes.remove(&seq) { Some(c) => (self.decodificar(seq, &c), b'V'), None => (self.decodificador.decodificar(None), b'v') };
                    for b in pcm.chunks_exact(MUESTRAS_BLOQUE) { self.bloques.push_back(b.to_vec()); self.origenes.push_back(o); self.empujados += 1 }
                    seq = seq.wrapping_add(1);
                }
            }
        }
        let referencia = nivel_de_referencia(&self.bloques);
        let Some(t) = &mut self.tapar else { return };
        if let Some(r) = referencia { t.referencia_db = Some(r) }
        let mut quitados = 0;
        while quitados < 250 && self.bloques.back().is_some_and(|b| es_silencio(b) || es_pausa(b, t.referencia_db)) { self.bloques.pop_back(); self.origenes.pop_back(); self.empujados -= 1; quitados += 1 }
        t.cola_recortada += quitados;
        // La junta: lo último del viejo tal como va a sonar.
        let n = self.bloques.len();
        let viejo: Vec<String> = self.bloques.iter().zip(self.origenes.iter()).skip(n.saturating_sub(15)).map(|(b, o)| format!("{:.0}{}", db(b), *o as char)).collect();
        self.junta = Some((format!("{} | recortados {quitados} |", viejo.join(" ")), Vec::new()));
        self.marcas.push_back(self.empujados);
    }

    /// En la ventana de tras un relevo, si falta `seq` y el siguiente que hay está
    /// 3 o más por delante: ese siguiente.
    fn salto_tras_relevo(&self, seq: u16) -> Option<u16> {
        // Sólo antes del primer paquete real del nuevo (revisión 35, c): un hueco
        // después puede ser un perdido de verdad, y para eso está el PLC.
        if !self.tapar.is_some_and(|t| t.principio > 0 && t.fase == 0) { return None }
        let d = self.paquetes.keys().map(|&s| s.wrapping_sub(seq)).filter(|d| *d < 32768).min()?;
        (d >= 3).then(|| seq.wrapping_add(d))
    }

    /// ¿Hay algún paquete al menos `n` por delante de `seq`?
    fn adelantado(&self, seq: u16, n: u16) -> bool { self.paquetes.keys().any(|&s| (n..32768).contains(&s.wrapping_sub(seq))) }

    /// Con `--tapar-relevo`: ¿era el último bloque sacado el primero del transporte nuevo?
    pub fn empalme(&self) -> bool { self.empalme_ultimo }

    fn subir(&mut self) { self.reserva_ms = (self.reserva_ms + 10).min(self.maxima_ms); }

    fn retrasada(&mut self, ahora_ms: f64) {
        if self.en_rafaga { return; }
        self.cuenta.retrasos += 1;
        if ahora_ms - self.ultimo_retraso_ms < REPETICION_MS { self.subir(); }
        self.ultimo_retraso_ms = ahora_ms;
        self.ultimo_vaciado_ms = ahora_ms;
    }

    fn vaciado(&mut self, ahora_ms: f64) {
        if !self.sonando { return; }
        self.sonando = false;
        self.ocultos_seguidos = 0;
        self.siguiente = None;
        self.cuenta.vaciados += 1;
        self.ultimo_vaciado_ms = ahora_ms;
        self.subir();
    }

    fn calmar(&mut self, ahora_ms: f64) {
        if ahora_ms - self.ultimo_vaciado_ms > self.calma_ms && self.reserva_ms > self.minima_ms {
            self.reserva_ms = self.reserva_ms.saturating_sub(10).max(self.minima_ms);
            self.ultimo_vaciado_ms = ahora_ms;
        }
    }

    fn cola_ms(&self) -> usize { self.bloques.len() * 10 + self.paquetes.len() * 20 }

    fn rellenar(&mut self, ahora_ms: f64) -> Vec<Emision> {
        let mut emitidos = vec![];
        if self.siguiente.is_none() { return emitidos; }
        if !self.sonando {
            // Con `--tapar-relevo` se suena en cuanto hay 300 ms (también al
            // arrancar: sin 2 s de silencio inicial); la reserva crece hasta el
            // objetivo estirando (elastico.rs).
            let umbral = if self.tapar.is_some() { (self.objetivo_ms() as usize).min(300) } else { self.objetivo_ms() as usize };
            if self.paquetes.len() * 20 < umbral { return emitidos; }
            self.sonando = true;
        }
        let bloques_objetivo = (self.objetivo_ms() as usize).div_ceil(10);
        while self.bloques.len() < bloques_objetivo {
            let seq = self.siguiente.unwrap();
            let (pcm, tipo, avanzar) = if let Some(carga) = self.paquetes.remove(&seq) {
                let pcm = self.decodificar(seq, &carga);
                self.recordar(seq, Resuelto::Real);
                self.ocultos_seguidos = 0;
                // Con `--tapar-relevo`: el silencio digital tras un relevo (hasta que el
                // Play hace efecto) y, con la reserva por debajo del objetivo, el de
                // entre canciones, se descartan: recuperan reserva sin estirar.
                let bajo = self.cola_ms() < self.objetivo_ms() as usize;
                if let Some(t) = &mut self.tapar {
                    t.principio = t.principio.saturating_sub(1);
                    let silencio = es_silencio(&pcm);
                    // en los 3 s de tras cada relevo. Spotify se para al
                    // conectar el transporte nuevo, que llega con 20-50 ms de música (la
                    // cabeza), la caída y la pausa hasta que el Play hace efecto; o con la
                    // pausa directamente. Se quita la pausa de antes de la cabeza y la de
                    // después (contigua a ella, hasta que vuelve la música), y donde se
                    // une la cabeza con la reanudación va un empalme (fundido de 5 ms).
                    // «Pausa» = silencio, o < −45 dBFS y ≥ 20 dB bajo la música de antes.
                    if t.principio > 0 && t.fase < 3 {
                        let pausa = silencio || es_pausa(&pcm, t.referencia_db);
                        let quitar = match t.fase {
                            0 if pausa => true,
                            0 => { t.fase = 1; t.cabeza = 1; false }
                            1 if pausa && t.cabeza <= 25 => { t.fase = 2; true }
                            1 if pausa => { t.fase = 3; false }
                            1 => { t.cabeza += 1; if t.cabeza > 25 { t.fase = 3 } false }
                            2 if pausa => true,
                            _ => { t.fase = 3; self.marcas.push_back(self.empujados); false }
                        };
                        if quitar && t.quitados_ventana < 150 {
                            t.quitados_ventana += 1;
                            if t.fase == 2 { t.pausa_tras_cabeza += 1 } else if silencio { t.silencio_descartado += 1 } else { t.inicio_bajo += 1 }
                            self.cuenta.recortados += 1;
                            self.siguiente = Some(seq.wrapping_add(1));
                            continue;
                        }
                    }
                    t.silencio_seguido = if silencio { t.silencio_seguido + 1 } else { 0 };
                    if silencio && bajo && t.silencio_seguido > 25 {
                        if t.silencio_seguido > 250 { t.silencio_largo += 1 } else { t.silencio_entre_canciones += 1 }
                        self.cuenta.recortados += 1;
                        self.siguiente = Some(seq.wrapping_add(1));
                        continue;
                    }
                }
                // Un paquete de silencio sobra sólo cuando la cola supera objetivo + 20 ms.
                if self.cola_ms() > self.objetivo_ms() as usize + 20 && es_silencio(&pcm) {
                    self.cuenta.recortados += 1;
                    self.siguiente = Some(seq.wrapping_add(1));
                    continue;
                }
                self.cuenta.sonados += 1;
                (pcm, TipoEmision::Real(seq), true)
            } else if let Some(siguiente) = self.salto_tras_relevo(seq) {
                // al principio del transporte nuevo, un hueco de 3 o más
                // números no se rellena con PLC (decae ~2 dB cada 10 ms: el «hipo» de
                // 110 ms); la consola no los mandó: se salta.
                if let Some(t) = &mut self.tapar { t.saltados += u64::from(siguiente.wrapping_sub(seq)) }
                self.siguiente = Some(siguiente);
                continue;
            } else if (self.bloques.is_empty() || (self.tapar.is_some() && self.adelantado(seq, 3)))
                && (!self.paquetes.is_empty() || self.ocultos_seguidos < MAX_OCULTOS_SEGUIDOS) {
                // Con `--tapar-relevo` un perdido no se espera hasta vaciar
                // los 3 s de reserva: si ya hay paquetes 3 por delante (60 ms), PLC.
                let pcm = self.decodificador.decodificar(None);
                self.cuenta.ocultados += 1;
                if !self.paquetes.is_empty() {
                    self.recordar(seq, Resuelto::Oculto);
                    self.ocultos_seguidos += 1;
                    (pcm, TipoEmision::PlcPerdida(seq), true)
                } else {
                    if self.ocultos_seguidos == 0 { self.retrasada(ahora_ms); }
                    self.cuenta.estirados += 1;
                    self.ocultos_seguidos += 1;
                    (pcm, TipoEmision::PlcEstirado(seq), false)
                }
            } else {
                if self.bloques.is_empty() { self.vaciado(ahora_ms); }
                return emitidos;
            };
            if avanzar { self.siguiente = Some(seq.wrapping_add(1)); }
            emitidos.push(Emision { tipo, primera_muestra: *pcm.first().unwrap_or(&0) });
            let origen = match tipo { TipoEmision::Real(_) => b'R', TipoEmision::PlcPerdida(_) => b'P', TipoEmision::PlcEstirado(_) => b'E' };
            for bloque in pcm.chunks_exact(MUESTRAS_BLOQUE) {
                let referencia = self.tapar.and_then(|t| t.referencia_db);
                if let Some((viejo, nuevo)) = &mut self.junta {
                    // El primero nuevo: ¿entra la música a mitad de nota (cerca del nivel de antes)?
                    if nuevo.is_empty() {
                        if let Some(r) = referencia { viejo.push_str(if db(bloque) >= r - 10.0 { " [entra a mitad de nota]" } else { " [entra desde abajo]" }) }
                    }
                    nuevo.push(format!("{:.0}{}", db(bloque), origen as char));
                    if nuevo.len() >= 15 {
                        let (viejo, nuevo) = self.junta.take().unwrap();
                        self.junta_lista = Some(format!("junta (dBFS y origen de cada 10 ms; R real, P PLC por perdido, E PLC por falta, V/v del viejo al reanclar): {viejo} {}", nuevo.join(" ")));
                    }
                }
                self.bloques.push_back(bloque.to_vec());
                self.origenes.push_back(origen);
                self.empujados += 1;
            }
        }
        self.calmar(ahora_ms);
        emitidos
    }
}

/// El mismo umbral int16 de `esSilencio`: todas las muestras en [-32, 32].
pub fn es_silencio(pcm: &[i16]) -> bool { pcm.iter().all(|muestra| muestra.abs_diff(0) <= 32) }

#[cfg(test)]
mod pruebas {
    use super::*;

    struct DecodificadorFalso;
    impl Decodificador for DecodificadorFalso {
        fn decodificar(&mut self, carga: Option<&[u8]>) -> Vec<i16> {
            // 20-127 → byte·257 (música, −16…−2 dBFS); 10-19 → byte·30 (música
            // suave, −41…−35 dBFS); 2-9 → byte·20 (la pausa de Spotify, −58…−46
            // dBFS); 1 → 20 (silencio, ±32); sin carga (PLC) → −1.
            let byte = carga.and_then(|c| c.first()).copied().unwrap_or(255);
            let v = match byte { 2..=9 => i16::from(byte) * 20, 10..=19 => i16::from(byte) * 30, 1 => 20, b => i16::from_le_bytes([b, b]) };
            vec![v; 960 * 2]
        }
    }
    /// con la reserva llena, libopus trabaja al llegar el paquete
    /// (hilo de red) y no en la llamada de la tarjeta, en el mismo orden.
    #[test]
    fn libopus_al_llegar_y_en_orden() {
        use std::sync::{Arc, Mutex};
        #[derive(Clone, Default)]
        struct Contado(Arc<Mutex<Vec<u8>>>);
        impl Decodificador for Contado {
            fn decodificar(&mut self, carga: Option<&[u8]>) -> Vec<i16> {
                let b = carga.and_then(|c| c.first()).copied().unwrap_or(255);
                self.0.lock().unwrap().push(b);
                vec![i16::from(b); 960 * 2]
            }
        }
        let registro = Contado::default();
        let mut r = Reserva::nueva(registro.clone(), 40, 40, 200, 60_000.0, 0.0);
        // Un paquete de más en la reserva: cada uno llega con ella llena, que es
        // cuando antes se decodificaba en la llamada de la tarjeta.
        for i in 0..3u16 { r.recibir(i, vec![i as u8], 0.0); }
        let mut en_llamadas = 0;
        let mut sonado = Vec::new();
        for i in 3..50u16 {
            for k in 0..2 {
                let antes = registro.0.lock().unwrap().len();
                if let Some(b) = r.consumir_bloque(f64::from(i) * 20.0 + f64::from(k) * 10.0).0 { sonado.push(b[0]) }
                en_llamadas += registro.0.lock().unwrap().len() - antes;
            }
            r.recibir(i, vec![i as u8], f64::from(i) * 20.0 + 15.0);
        }
        let orden = registro.0.lock().unwrap().clone();
        assert_eq!(orden, (0..orden.len() as u8).collect::<Vec<_>>(), "libopus, en orden y sin PLC");
        assert_eq!(en_llamadas, 0, "ninguna decodificación en la llamada de la tarjeta");
        assert!(r.cuenta.adelantados > 40, "{:?}", r.cuenta);
        // Lo que suena, igual que antes: cada paquete, dos bloques, en orden.
        assert!(sonado.windows(2).all(|w| w[1] == w[0] || w[1] == w[0] + 1), "{sonado:?}");
    }

    /// `--tapar-relevo`: tras un relevo, el silencio digital del
    /// principio del transporte nuevo se descarta y su primer bloque con música
    /// sale marcado como empalme, después de todos los viejos.
    #[test]
    fn tapar_relevo_descarta_silencio_y_marca_el_empalme() {
        let mut r = Reserva::nueva(DecodificadorFalso, 200, 200, 400, 60_000.0, 0.0);
        r.tapar = Some(Default::default());
        for s in 0..10u16 { r.recibir(s, vec![70], f64::from(s) * 20.0); }
        for i in 0..5 {
            assert!(r.consumir_bloque(200.0 + f64::from(i) * 10.0).0.is_some());
            assert!(!r.empalme());
        }
        let viejos = r.bloques_pendientes();
        r.reanclar();
        for (i, s) in (500u16..510).enumerate() { r.recibir(s, vec![if i < 5 { 0 } else { 90 }], 300.0); }
        for _ in 0..viejos {
            let (b, _) = r.consumir_bloque(300.0);
            assert_eq!(b.unwrap()[0], i16::from_le_bytes([70, 70]));
            assert!(!r.empalme());
        }
        let (b, _) = r.consumir_bloque(310.0);
        assert_eq!(b.unwrap()[0], i16::from_le_bytes([90, 90]));
        assert!(r.empalme());
        assert_eq!(r.tapar.unwrap().silencio_descartado, 5);
    }

    /// con el objetivo real (2000 ms) la reserva se llena entera; con
    /// la distancia máxima fija de 50 paquetes se vaciaba al pasar de 1 s.
    #[test]
    fn tapar_relevo_con_2_s_se_llena() {
        let mut r = Reserva::nueva(DecodificadorFalso, 2000, 2000, 2000, 60_000.0, 0.0);
        r.tapar = Some(Default::default());
        // Suena con 300 ms, sin esperar a los 2 s.
        for s in 0..15u16 { r.recibir(s, vec![70], f64::from(s) * 20.0); }
        assert!(r.sonando && r.bloques_pendientes() > 0);
        // Llegan 4 s de golpe sin que se toque nada: 2 s decodificados y 2 s por
        // decodificar, 100 paquetes por delante de `siguiente`. No se tira ninguno.
        for s in 15..200u16 { r.recibir(s, vec![70], 300.0); }
        assert_eq!(r.nivel_ms(), 200 * 20);
        assert_eq!(r.cuenta.recortados, 0);
    }

    /// al llegar el transporte nuevo, la cola de la pausa del viejo
    /// (la caída de −20 a −49 dB y su decaimiento) se recorta.
    #[test]
    fn tapar_relevo_recorta_la_cola_de_la_pausa() {
        let mut r = Reserva::nueva(DecodificadorFalso, 2000, 2000, 2000, 60_000.0, 0.0);
        r.tapar = Some(Default::default());
        // Música (100·257 ≈ −2 dB) y la cola: 3 (≈ −32 dB), 2, 1.
        let cargas = [100u8, 100, 100, 100, 100, 100, 100, 100, 100, 100, 100, 100, 100, 100, 100, 100, 3, 2, 1];
        for (s, c) in cargas.iter().enumerate() { r.recibir(s as u16, vec![*c], 0.0); }
        let antes = r.bloques_pendientes();
        r.reanclar();
        assert_eq!(r.bloques_pendientes(), antes - 6);
        assert_eq!(r.tapar.unwrap().cola_recortada, 6);
        // Sin caída, no se recorta nada.
        let mut q = Reserva::nueva(DecodificadorFalso, 2000, 2000, 2000, 60_000.0, 0.0);
        q.tapar = Some(Default::default());
        for s in 0..20u16 { q.recibir(s, vec![100 - s as u8], 0.0); }
        let antes = q.bloques_pendientes();
        q.reanclar();
        assert_eq!(q.bloques_pendientes(), antes);
    }

    /// el caso real: 1,5 s de reserva; el transporte nuevo empieza
    /// con la cola de la pausa (5 paquetes bajos, no silencio) y luego música.
    /// La cola se descarta y el primer bloque nuevo que suena es música.
    #[test]
    fn tapar_relevo_descarta_el_inicio_bajo_del_transporte_nuevo() {
        let mut r = Reserva::nueva(DecodificadorFalso, 2000, 2000, 2000, 60_000.0, 0.0);
        r.tapar = Some(Default::default());
        for s in 0..75u16 { r.recibir(s, vec![100], 0.0); }
        // El hueco se come 1,2 s de la reserva.
        for _ in 0..120 { r.consumir_bloque(10.0); }
        let viejos = r.bloques_pendientes();
        r.reanclar();
        assert_eq!(r.bloques_pendientes(), viejos);
        for (i, s) in (900u16..910).enumerate() { r.recibir(s, vec![if i < 5 { 5 - i as u8 } else { 100 }], 20.0); }
        for _ in 0..viejos { r.consumir_bloque(30.0); assert!(!r.empalme()); }
        let (b, _) = r.consumir_bloque(40.0);
        assert_eq!(b.unwrap()[0], i16::from_le_bytes([100, 100]));
        assert!(r.empalme());
        let t = r.tapar.unwrap();
        assert_eq!((t.inicio_bajo, t.silencio_descartado), (4, 1));
    }

    /// patrón del relevo #3: la cola de la pausa en el transporte
    /// VIEJO, larga (decae y se queda baja 600 ms), y parte de ella aún sin
    /// decodificar tras un paquete perdido. Se decodifica y se quita entera.
    #[test]
    fn tapar_relevo_quita_la_cola_larga_del_viejo() {
        let mut r = Reserva::nueva(DecodificadorFalso, 2000, 2000, 2000, 60_000.0, 0.0);
        r.tapar = Some(Default::default());
        let mut s = 0u16;
        for _ in 0..60 { r.recibir(s, vec![100], 0.0); s += 1; }
        for c in [5u8, 4, 3, 2] { r.recibir(s, vec![c], 0.0); s += 1; }
        s += 1; // un paquete perdido: lo de después espera sin decodificar
        for _ in 0..30 { r.recibir(s, vec![1], 0.0); s += 1; }
        let musica = 60 * 2;
        r.reanclar();
        assert_eq!(r.bloques_pendientes(), musica);
        // Lo que no recorta el relevo ya lo quitó el silencio de más de 0,5 s seguidos.
        let t = r.tapar.unwrap();
        assert_eq!((t.cola_recortada + 2 * t.silencio_entre_canciones) as usize, 8 + 2 + 60);
    }

    /// dos relevos seguidos; en los dos se quita lo bajo del
    /// principio del nuevo, pero no un paquete suelto bajo dentro de su música
    /// (quitarlo sería un clic o comerse música), y cada uno cuenta como relevo.
    #[test]
    fn tapar_relevo_funciona_en_todos_los_relevos() {
        let mut r = Reserva::nueva(DecodificadorFalso, 2000, 2000, 2000, 60_000.0, 0.0);
        r.tapar = Some(Default::default());
        for s in 0..60u16 { r.recibir(s, vec![100], 0.0); }
        for (relevo, base) in [(1u64, 1000u16), (2, 2000)] {
            r.reanclar();
            let cargas = [3u8, 2, 1, 100, 100, 1, 100, 100];
            for (i, c) in cargas.iter().enumerate() { r.recibir(base + i as u16, vec![*c], 0.0); }
            // Suena mientras llega (sin consumir, la reserva no deja decodificar más).
            for i in 0..50u16 { r.recibir(base + 8 + i, vec![100], 0.0); r.consumir_bloque(0.0); r.consumir_bloque(0.0); }
            let t = r.tapar.unwrap();
            // Los 3 del principio (2 bajos y 1 silencio) sí; el suelto de en medio, no.
            assert_eq!((t.relevos, t.inicio_bajo, t.silencio_descartado), (relevo, 2 * relevo, relevo));
        }
    }

    /// una intro suave del transporte nuevo (−39 dBFS, ≥ 20 dB bajo la
    /// música de antes pero por encima de −45) no es la pausa: no se quita.
    #[test]
    fn tapar_relevo_no_se_come_una_intro_suave() {
        let mut r = Reserva::nueva(DecodificadorFalso, 2000, 2000, 2000, 60_000.0, 0.0);
        r.tapar = Some(Default::default());
        for s in 0..60u16 { r.recibir(s, vec![100], 0.0); }
        r.reanclar();
        let antes = r.bloques_pendientes();
        for s in 500..510u16 { r.recibir(s, vec![12], 0.0); }
        assert_eq!(r.bloques_pendientes(), antes + 20);
        assert_eq!(r.tapar.unwrap().inicio_bajo, 0);
    }

    /// si antes de la primera música del transporte nuevo
    /// faltan 3 números (la consola no los mandó), se saltan: nada de PLC
    /// decayendo en la junta. Después de la primera música, un hueco es un
    /// perdido de verdad y lleva su PLC.
    #[test]
    fn tapar_relevo_salta_el_hueco_del_principio_sin_plc() {
        let mut r = Reserva::nueva(DecodificadorFalso, 2000, 2000, 2000, 60_000.0, 0.0);
        r.tapar = Some(Default::default());
        for s in 0..60u16 { r.recibir(s, vec![100], 0.0); }
        r.reanclar();
        let antes = r.bloques_pendientes();
        // Antes de la primera música: un silencio y luego faltan 3 números.
        r.recibir(500, vec![1], 0.0);
        for s in 504..510u16 { r.recibir(s, vec![100], 0.0); }
        assert_eq!(r.bloques_pendientes(), antes + 6 * 2);
        assert_eq!((r.tapar.unwrap().saltados, r.cuenta.ocultados), (3, 0));
    }

    /// el patrón medido: lo nuevo trae 40 ms de música (la cabeza),
    /// luego la caída y la pausa de Spotify (−46…−58 dBFS) y luego la música otra
    /// vez. La pausa se quita y la reanudación va marcada como empalme (para el
    /// fundido de 5 ms con la cabeza).
    #[test]
    fn tapar_relevo_quita_la_pausa_tras_la_cabeza() {
        let mut r = Reserva::nueva(DecodificadorFalso, 2000, 2000, 2000, 60_000.0, 0.0);
        r.tapar = Some(Default::default());
        for s in 0..60u16 { r.recibir(s, vec![100], 0.0); }
        r.reanclar();
        let viejos = r.bloques_pendientes();
        let cargas = [100u8, 90, 9, 8, 7, 6, 5, 4, 3, 2, 100, 100, 100, 100];
        for (i, c) in cargas.iter().enumerate() { r.recibir(500 + i as u16, vec![*c], 0.0); }
        let t = r.tapar.unwrap();
        assert_eq!((t.pausa_tras_cabeza, t.inicio_bajo), (8, 0));
        assert_eq!(t.pausa(), Pausa::Musica, "la música volvió");
        // A media pausa, sí.
        let mut r2 = Reserva::nueva(DecodificadorFalso, 2000, 2000, 2000, 60_000.0, 0.0);
        r2.tapar = Some(Default::default());
        for s in 0..60u16 { r2.recibir(s, vec![100], 0.0); }
        r2.reanclar();
        for (i, c) in [100u8, 90].iter().enumerate() { r2.recibir(500 + i as u16, vec![*c], 0.0); }
        assert!(!r2.tapar.unwrap().en_pausa());
        r2.recibir(502, vec![9], 0.0);
        assert!(r2.tapar.unwrap().en_pausa());
        assert_eq!(r.bloques_pendientes(), viejos + (2 + 4) * 2);
        let mut marcados = Vec::new();
        for i in 0..r.bloques_pendientes() { r.consumir_bloque(0.0); if r.empalme() { marcados.push(i) } }
        // El primero de lo nuevo (junta con lo viejo) y el de la reanudación.
        assert_eq!(marcados, vec![viejos, viejos + 2 * 2]);
    }

    /// sin cabeza, la pausa cuenta desde los
    /// 300 ms; una cabeza suelta entre dos tramos de pausa no es la vuelta; y si
    /// la ventana se acaba sin música es `Fuera`, no `Musica`.
    #[test]
    fn estado_de_la_pausa_tras_el_relevo() {
        let mut r = Reserva::nueva(DecodificadorFalso, 2000, 2000, 2000, 60_000.0, 0.0);
        r.tapar = Some(Default::default());
        assert_eq!(r.tapar.unwrap().pausa(), Pausa::Fuera);
        for s in 0..60u16 { r.recibir(s, vec![100], 0.0); }
        r.reanclar();
        let mut seq = 500u16;
        let mut paquete = |r: &mut Reserva<DecodificadorFalso>, c: u8| { r.recibir(seq, vec![c], 0.0); seq += 1; r.tapar.unwrap().pausa() };
        for _ in 0..14 { assert_eq!(paquete(&mut r, 9), Pausa::Esperando) }
        assert_eq!(paquete(&mut r, 9), Pausa::EnPausa);
        // Un paquete con nivel (fase 1) y otra vez pausa (fase 2).
        assert_eq!(paquete(&mut r, 100), Pausa::Esperando);
        assert_eq!(paquete(&mut r, 9), Pausa::EnPausa);
        // La ventana se acaba sin música.
        let mut ultimo = Pausa::EnPausa;
        for _ in 0..200 { ultimo = paquete(&mut r, 9) }
        assert_eq!(ultimo, Pausa::Fuera);
        // En el relevo siguiente, la música de verdad tras la pausa sí es `Musica`.
        r.reanclar();
        for c in [100u8, 9, 9, 100] { ultimo = paquete(&mut r, c) }
        assert_eq!(ultimo, Pausa::Musica);
    }
}
