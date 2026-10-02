//! `--tapar-relevo`: la salida elástica del modo música.
//!
//! WSOLA (Verhelst y Roelands, 1993) en tiempo real, entre la reserva y la
//! tarjeta: ventanas de Hann de 30 ms solapadas al 50 % (salida cada 15 ms).
//! Para tocar un `ritmo` más lento (0,025 = 2,5 %), cada ventana se toma de la
//! entrada 15/(1+ritmo) ms después de la anterior, desplazada ±10 ms hasta
//! donde mejor continúa lo ya tocado (correlación estéreo conjunta). Sin
//! cambiar el tono. Con ritmo 0 la ventana cae justo donde toca y la suma de
//! las dos Hann es 1: la salida es la entrada, sin tocarla.
//!
//! En un relevo, lo viejo y lo nuevo se unen en la entrada antes de las
//! ventanas: los últimos 5 ms de lo viejo (aún no leídos por ninguna) se funden
//! con coseno alzado con lo nuevo, desde el desfase de 0-5 ms que mejor encaja
//! (si encaja: correlación ≥ 0,5). Pasado a la entrada tal cual, el corte
//! sonaría como clic.
//!
//! Estéreo intercalado a 48 kHz. Las cifras salen en atómicos (`CIFRAS`): el
//! hilo de audio no registra.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

/// 10 ms a 48 kHz, en muestras por canal.
pub const TRAMA: usize = 480;
/// Ventana de 30 ms y salto de síntesis de 15 ms (50 %).
const N: usize = 1440;
const HS: usize = N / 2;
/// Búsqueda de ±10 ms.
const DELTA: usize = 480;
/// Fundido y desfase máximo del empalme: 5 ms.
const FUNDIDO: usize = 240;

/// Lo que mide el elástico, para el registro (lo lee otro hilo).
/// `entrada` y `salida`: muestras en los búferes del elástico.
pub struct Cifras { pub insertadas: AtomicU64, pub empalmes: AtomicU64, pub sin_empalme: AtomicU64, pub ritmo_ppm: AtomicU64, pub nivel_ms: AtomicU64, pub entrada: AtomicU64, pub salida: AtomicU64 }
pub static CIFRAS: Cifras = Cifras { insertadas: AtomicU64::new(0), empalmes: AtomicU64::new(0), sin_empalme: AtomicU64::new(0), ritmo_ppm: AtomicU64::new(0), nivel_ms: AtomicU64::new(0), entrada: AtomicU64::new(0), salida: AtomicU64::new(0) };

pub struct Elastico {
    /// La entrada pendiente (estéreo intercalado); `base` es el índice absoluto
    /// (en muestras por canal) de su primera muestra, `total` el de la siguiente a llegar.
    entrada: VecDeque<f32>,
    base: usize,
    total: usize,
    /// Dónde debería empezar la ventana siguiente (absoluto, con decimales).
    pos: f64,
    /// Dónde empezó la anterior.
    ultimo: Option<usize>,
    /// La mitad final de la ventana anterior, ya multiplicada por Hann.
    cola: Vec<f32>,
    salida: VecDeque<i16>,
    hann: Vec<f32>,
    /// Fracción de muestras a añadir (0,02 = tocar un 2 % más lento).
    pub ritmo: f64,
    /// Muestras por canal añadidas (salida menos entrada consumida).
    pub insertadas: u64,
    producidas: u64,
    pub empalmes: u64,
    pub sin_empalme: u64,
}

impl Default for Elastico {
    fn default() -> Self {
        // Hann periódica: dos solapadas al 50 % suman exactamente 1.
        let hann = (0..N).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / N as f64).cos()) as f32).collect();
        Self { entrada: VecDeque::new(), base: 0, total: 0, pos: 0.0, ultimo: None, cola: vec![0.0; 2 * HS], salida: VecDeque::new(), hann,
            ritmo: 0.0, insertadas: 0, producidas: 0, empalmes: 0, sin_empalme: 0 }
    }
}

impl Elastico {
    /// Muestras por canal ya listas más las de entrada aún sin tocar.
    pub fn muestras_en_cola(&self) -> usize { self.salida.len() / 2 + self.total.saturating_sub(self.pos as usize) }

    /// Saca `n` muestras por canal (estéreo intercalado), si las hay.
    pub fn sacar(&mut self, n: usize) -> Option<Vec<i16>> {
        if self.salida.len() < 2 * n { return None }
        Some(self.salida.drain(..2 * n).collect())
    }

    /// Un bloque de la reserva. `empalme`: es el primero del transporte nuevo
    /// (sólo se cuenta: WSOLA lo funde como a cualquier otro).
    pub fn empujar(&mut self, bloque: &[i16], empalme: bool) {
        if !empalme {
            self.entrada.extend(bloque.iter().map(|&x| f32::from(x)));
            self.total += bloque.len() / 2;
            self.procesar();
            return;
        }
        let nuevo: Vec<f32> = bloque.iter().map(|&x| f32::from(x)).collect();
        // Lo último de la entrada no lo ha leído ninguna ventana (`procesar` deja
        // FUNDIDO de margen), así que se puede fundir ahí.
        if self.entrada.len() >= 2 * FUNDIDO && nuevo.len() >= 2 * (2 * FUNDIDO) {
            let ini = self.entrada.len() - 2 * FUNDIDO;
            let viejo: Vec<f32> = self.entrada.range(ini..).copied().collect();
            let corr = |d: usize| {
                let (mut ab, mut aa, mut bb) = (0.0f64, 0.0f64, 0.0f64);
                for k in 0..2 * FUNDIDO { let (x, y) = (f64::from(viejo[k]), f64::from(nuevo[2 * d + k])); ab += x * y; aa += x * x; bb += y * y }
                if aa < 1.0 || bb < 1.0 { 0.0 } else { ab / (aa * bb).sqrt() }
            };
            let (mut mejor, mut d) = (0.5, 0);
            for c in 0..=FUNDIDO { let v = corr(c); if v > mejor { mejor = v; d = c } }
            for k in 0..FUNDIDO {
                let w = (0.5 - 0.5 * (std::f64::consts::PI * (k as f64 + 0.5) / FUNDIDO as f64).cos()) as f32;
                for c in 0..2 { self.entrada[ini + 2 * k + c] = viejo[2 * k + c] * (1.0 - w) + nuevo[2 * (d + k) + c] * w }
            }
            self.entrada.extend(&nuevo[2 * (d + FUNDIDO)..]);
            self.total += nuevo.len() / 2 - d - FUNDIDO;
            self.empalmes += 1;
        } else {
            self.sin_empalme += 1;
            self.entrada.extend(nuevo);
            self.total += bloque.len() / 2;
        }
        self.procesar();
    }

    fn x(&self, i: usize, c: usize) -> f32 { self.entrada[2 * (i - self.base) + c] }

    /// Correlación estéreo conjunta de `x[a..a+n]` con `x[b..b+n]`, cada `paso` muestras.
    fn correlacion(&self, a: usize, b: usize, n: usize, paso: usize) -> f64 {
        let (mut ab, mut aa, mut bb) = (0.0f64, 0.0f64, 0.0f64);
        let mut k = 0;
        while k < n {
            for c in 0..2 {
                let (x, y) = (f64::from(self.x(a + k, c)), f64::from(self.x(b + k, c)));
                ab += x * y;
                aa += x * x;
                bb += y * y;
            }
            k += paso;
        }
        if aa < 1.0 || bb < 1.0 { return 0.0 }
        ab / (aa * bb).sqrt()
    }

    /// El inicio de ventana en `centro ± DELTA` que mejor continúa `natural`:
    /// primero cada 4 muestras (correlación cada 4), luego ±3 fino.
    fn buscar(&self, centro: usize, natural: usize) -> usize {
        let desde = centro.saturating_sub(DELTA).max(self.base);
        let hasta = centro + DELTA;
        // con poca energía la correlación es 0 en
        // todas partes; partir de 0 y de `natural` hace que se quede en su sitio
        // en vez de saltar a −10 ms (doble ataque al volver el sonido).
        let (mut mejor, mut c_mejor) = (0.0, natural.clamp(desde, hasta));
        let mut c = desde;
        while c <= hasta {
            let v = self.correlacion(c, natural, HS, 4);
            if v > mejor { mejor = v; c_mejor = c }
            c += 4;
        }
        if mejor <= 0.0 { return c_mejor }
        let grueso = c_mejor;
        for c in grueso.saturating_sub(3).max(desde)..=(grueso + 3).min(hasta) {
            let v = self.correlacion(c, natural, HS, 1);
            if v > mejor { mejor = v; c_mejor = c }
        }
        c_mejor
    }

    fn procesar(&mut self) {
        loop {
            // (Revisión 35, b) Con ritmo 0 se vuelve a la posición natural: la
            // ventana cae otra vez justo en su sitio y la salida vuelve a ser la entrada.
            if self.ritmo == 0.0 { if let Some(u) = self.ultimo { self.pos = (u + HS) as f64 } }
            let centro = self.pos.round() as usize;
            let natural = self.ultimo.map_or(centro, |u| u + HS);
            // Los últimos FUNDIDO de la entrada no los lee ninguna ventana: ahí se
            // funde un empalme (revisión 35, punto 4: sin este margen, sí se leían).
            if centro.max(natural) + DELTA + N + FUNDIDO > self.total { return }
            // Sin estirar, la ventana cae justo en su sitio: ni se busca.
            let c = if self.ultimo.is_none() || natural == centro { natural } else { self.buscar(centro, natural) };
            for i in 0..N {
                let w = self.hann[i];
                for canal in 0..2 {
                    let s = self.x(c + i, canal) * w;
                    if i < HS {
                        let v = self.cola[2 * i + canal] + s;
                        self.salida.push_back(v.round().clamp(-32768.0, 32767.0) as i16);
                    } else {
                        self.cola[2 * (i - HS) + canal] = s;
                    }
                }
            }
            self.ultimo = Some(c);
            self.pos += HS as f64 / (1.0 + self.ritmo);
            // Lo conseguido (revisión 35, punto 6): muestras que salen frente a
            // muestras de entrada que la posición ya ha dejado atrás.
            self.producidas += HS as u64;
            self.insertadas = self.producidas.saturating_sub(self.pos as u64);
            // Lo que ya no puede hacer falta: antes de la búsqueda y de la continuación.
            let olvidar = (self.pos as usize).saturating_sub(DELTA + 1).min(c + HS).saturating_sub(self.base);
            self.entrada.drain(..2 * olvidar.min(self.entrada.len() / 2));
            self.base += olvidar;
        }
    }

    /// Las cifras, para el registro (otro hilo las lee).
    pub fn publicar(&self, nivel_ms: usize) {
        CIFRAS.insertadas.store(self.insertadas, Ordering::Relaxed);
        CIFRAS.empalmes.store(self.empalmes, Ordering::Relaxed);
        CIFRAS.sin_empalme.store(self.sin_empalme, Ordering::Relaxed);
        CIFRAS.ritmo_ppm.store((self.ritmo * 1e6) as u64, Ordering::Relaxed);
        CIFRAS.nivel_ms.store(nivel_ms as u64, Ordering::Relaxed);
        CIFRAS.entrada.store(self.entrada.len() as u64, Ordering::Relaxed);
        CIFRAS.salida.store(self.salida.len() as u64, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    /// Música de prueba: tres tonos (polifonía) estéreo desde la muestra `desde`.
    fn acorde(desde: usize, n: usize, fase: f64) -> Vec<i16> {
        (desde..desde + n).flat_map(|i| {
            let t = i as f64 / 48_000.0;
            let f = |hz: f64, a: f64| a * (2.0 * std::f64::consts::PI * hz * t + fase).sin();
            let l = f(220.0, 4000.0) + f(277.2, 3000.0) + f(329.6, 2500.0);
            let r = f(220.0, 3000.0) + f(277.2, 4000.0) + f(440.0, 2000.0);
            [l as i16, r as i16]
        }).collect()
    }

    fn salto_maximo(s: &[i16]) -> i32 { s.chunks(2).collect::<Vec<_>>().windows(2).map(|w| (i32::from(w[1][0]) - i32::from(w[0][0])).abs()).max().unwrap() }

    fn tocar(e: &mut Elastico) -> Vec<i16> {
        let mut todo = Vec::new();
        while let Some(b) = e.sacar(TRAMA) { todo.extend(b) }
        todo
    }

    #[test]
    fn sin_ritmo_la_salida_es_la_entrada() {
        let mut e = Elastico::default();
        let entrada: Vec<i16> = (0..100).flat_map(|i| acorde(i * TRAMA, TRAMA, 0.0)).collect();
        let mut salida = Vec::new();
        for b in entrada.chunks(2 * TRAMA) { e.empujar(b, false); salida.extend(tocar(&mut e)) }
        // Tras el arranque (la primera media ventana sube desde 0), idéntica ±1.
        let n = salida.len().min(entrada.len());
        assert!(n > entrada.len() * 9 / 10);
        for i in 2 * HS..n { assert!((i32::from(salida[i]) - i32::from(entrada[i])).abs() <= 1, "muestra {i}") }
        assert_eq!(e.insertadas, 0);
    }

    #[test]
    fn estira_lo_pedido_sin_clic() {
        for ritmo in [0.025, 0.04] {
            let mut e = Elastico { ritmo, ..Default::default() };
            let mut salida = Vec::new();
            for i in 0..500 { e.empujar(&acorde(i * TRAMA, TRAMA, 0.0), false); salida.extend(tocar(&mut e)) }
            // 5 s de entrada: lo añadido es el ritmo pedido (±10 %), no menos.
            let pedido = 500.0 * TRAMA as f64 * ritmo;
            let conseguido = e.insertadas as f64;
            assert!((conseguido - pedido).abs() < pedido * 0.1, "{ritmo}: {conseguido} de {pedido}");
            // El acorde cambia como mucho ~2·π·440·4000/48000 + … ≈ 600 por muestra.
            assert!(salto_maximo(&salida) < 1200, "{ritmo}: salto {}", salto_maximo(&salida));
        }
    }

    #[test]
    fn empalme_sin_clic() {
        let mut e = Elastico::default();
        let mut salida = Vec::new();
        for i in 0..20 { e.empujar(&acorde(i * TRAMA, TRAMA, 0.0), false); salida.extend(tocar(&mut e)) }
        // Lo nuevo sigue la música con otra fase (un corte en mitad de ciclo).
        for i in 0..20 { e.empujar(&acorde(i * TRAMA, TRAMA, 2.0), i == 0); salida.extend(tocar(&mut e)) }
        assert_eq!(e.empalmes, 1);
        assert!(salto_maximo(&salida) < 1200, "salto {}", salto_maximo(&salida));
    }

    /// Revisión 35, b: tras estirar, con ritmo 0 la salida vuelve a ser la entrada (desplazada).
    #[test]
    fn vuelve_a_ser_transparente_tras_estirar() {
        let mut e = Elastico { ritmo: 0.04, ..Default::default() };
        let mut salida = Vec::new();
        for i in 0..100 { e.empujar(&acorde(i * TRAMA, TRAMA, 0.0), false); salida.extend(tocar(&mut e)) }
        e.ritmo = 0.0;
        let desde = salida.len();
        for i in 100..300 { e.empujar(&acorde(i * TRAMA, TRAMA, 0.0), false); salida.extend(tocar(&mut e)) }
        let extra = e.insertadas as usize;
        let entrada: Vec<i16> = (0..300).flat_map(|i| acorde(i * TRAMA, TRAMA, 0.0)).collect();
        // Pasados un par de ventanas tras volver a 0, salida[k] = entrada[k − extra].
        for k in (desde / 2 + 2 * N)..(salida.len() / 2) {
            for c in 0..2 { assert!((i32::from(salida[2 * k + c]) - i32::from(entrada[2 * (k - extra) + c])).abs() <= 1, "muestra {k}") }
        }
    }

    /// Revisión 35, a: en silencio no se busca a −10 ms (se queda en su sitio).
    #[test]
    fn en_silencio_no_salta() {
        let mut e = Elastico { ritmo: 0.04, ..Default::default() };
        for _ in 0..50 { e.empujar(&[0i16; 2 * TRAMA], false); tocar(&mut e); }
        let antes = e.ultimo.unwrap();
        e.empujar(&[0i16; 2 * TRAMA], false);
        let despues = e.ultimo.unwrap();
        assert!(despues + DELTA / 2 > antes + HS, "saltó atrás: {antes} → {despues}");
    }
}
