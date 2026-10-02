//! `--micro[=<nombre>]`: el micro del PC hacia la
//! consola por el canal ChatAudio de la ruta nativa.
//! Captura con cpal (el dispositivo cuyo nombre contenga `<nombre>`; sin él,
//! el predeterminado), mezcla a mono, remuestreo a 24 kHz y Opus de 20 ms
//! (VoIP), como el formato que la consola acepta en el AudioAck de ChatAudio.
//!
//! Dos hilos: el de cpal (el flujo no se puede mover entre hilos) sólo copia
//! muestras a un canal; otro remuestrea y codifica. La sesión saca tramas con
//! `siguiente()` cada 20 ms; la cola guarda 5 como mucho (100 ms): si la red
//! va más lenta que el micro, se tira la más vieja, no se acumula retraso.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::registro::registrar;

pub const FRECUENCIA: u32 = 24_000;
/// 20 ms a 24 kHz.
pub const MUESTRAS_TRAMA: usize = 480;
const COLA_MAXIMA: usize = 5;

#[derive(Default)]
pub struct Micro {
    cola: Mutex<VecDeque<Vec<u8>>>,
    /// Suma de cuadrados ×1e6 y muestras desde la última lectura del nivel.
    cuadrados: AtomicU64,
    muestras: AtomicU64,
    pub codificadas: AtomicU64,
    pub tiradas: AtomicU64,
}

impl Micro {
    /// Antes de que la consola abra ChatAudio: lo codificado se tira sin contar.
    pub fn descartar(&self) { if let Ok(mut c) = self.cola.lock() { c.clear() } }

    /// La trama Opus más vieja de la cola, si hay.
    pub fn siguiente(&self) -> Option<Vec<u8>> { self.cola.lock().ok()?.pop_front() }

    /// Nivel RMS en dBFS desde la última llamada (None si no llegó nada).
    pub fn nivel_dbfs(&self) -> Option<f64> {
        let (c, n) = (self.cuadrados.swap(0, Ordering::Relaxed), self.muestras.swap(0, Ordering::Relaxed));
        (n > 0).then(|| 10.0 * ((c as f64 / 1e6) / n as f64).max(1e-12).log10())
    }

    fn encolar(&self, opus: Vec<u8>) {
        self.codificadas.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut c) = self.cola.lock() {
            if c.len() >= COLA_MAXIMA { c.pop_front(); self.tiradas.fetch_add(1, Ordering::Relaxed); }
            c.push_back(opus);
        }
    }

    fn medir(&self, bloque: &[f32]) {
        let c: f64 = bloque.iter().map(|&x| f64::from(x) * f64::from(x)).sum();
        self.cuadrados.fetch_add((c * 1e6) as u64, Ordering::Relaxed);
        self.muestras.fetch_add(bloque.len() as u64, Ordering::Relaxed);
    }
}

/// Remuestreo lineal a 24 kHz (para la voz basta), con estado entre bloques.
pub struct Remuestreo { paso: f64, pos: f64, previa: f32 }

impl Remuestreo {
    pub fn nuevo(origen: u32) -> Self { Self { paso: f64::from(origen) / f64::from(FRECUENCIA), pos: 0.0, previa: 0.0 } }

    /// `pos` es la posición de la próxima muestra de salida, relativa a la
    /// muestra previa (índice -1 = `previa`, 0 = `entrada[0]`…).
    pub fn procesar(&mut self, entrada: &[f32], salida: &mut Vec<f32>) {
        let muestra = |i: isize| if i < 0 { self.previa } else { entrada[i as usize] };
        let n = entrada.len() as f64;
        while self.pos < n - 1.0 {
            let base = self.pos.floor();
            let f = (self.pos - base) as f32;
            let (a, b) = (muestra(base as isize), muestra(base as isize + 1));
            salida.push(a + (b - a) * f);
            self.pos += self.paso;
        }
        if let Some(&u) = entrada.last() { self.previa = u }
        self.pos -= n;
    }
}

/// Abre el micro y arranca la captura. Devuelve el `Micro` que lee la sesión.
pub fn abrir(nombre: &str, t0: Instant) -> Result<Arc<Micro>, Box<dyn std::error::Error + Send + Sync>> {
    let micro = Arc::new(Micro::default());
    let (tx, rx) = mpsc::sync_channel::<Vec<f32>>(64);
    let (listo_tx, listo_rx) = mpsc::channel::<Result<(String, u32), String>>();
    let nombre = nombre.to_owned();
    // El hilo de cpal: abre el flujo y lo mantiene vivo.
    std::thread::Builder::new().name("micro-cpal".into()).spawn(move || {
        let abierto = (|| -> Result<(cpal::Stream, String, u32), String> {
            let host = cpal::default_host();
            let dispositivo = if nombre.is_empty() { host.default_input_device() } else {
                host.input_devices().map_err(|e| e.to_string())?.find(|d| d.name().is_ok_and(|n| n.to_lowercase().contains(&nombre.to_lowercase())))
            };
            let Some(d) = dispositivo else {
                let hay = host.input_devices().map(|ds| ds.filter_map(|d| d.name().ok()).collect::<Vec<_>>().join(" · ")).unwrap_or_default();
                return Err(format!("no hay micro «{nombre}» (hay: {hay})"));
            };
            let config = d.default_input_config().map_err(|e| e.to_string())?;
            let (canales, frecuencia) = (usize::from(config.channels()), config.sample_rate().0);
            let error = move |e| eprintln!("micro: {e}");
            // Mezcla a mono y manda; si el codificador va atrasado, se tira (no bloquear a cpal).
            let mono = move |muestras: Vec<f32>| -> Vec<f32> { muestras.chunks(canales).map(|c| c.iter().sum::<f32>() / c.len() as f32).collect() };
            let flujo = match config.sample_format() {
                cpal::SampleFormat::F32 => { let tx = tx.clone(); d.build_input_stream(&config.into(), move |m: &[f32], _: &_| { let _ = tx.try_send(mono(m.to_vec())); }, error, None) }
                cpal::SampleFormat::I16 => { let tx = tx.clone(); d.build_input_stream(&config.into(), move |m: &[i16], _: &_| { let _ = tx.try_send(mono(m.iter().map(|&x| f32::from(x) / 32768.0).collect())); }, error, None) }
                f => return Err(format!("formato de muestra {f:?} sin soporte")),
            }.map_err(|e| e.to_string())?;
            flujo.play().map_err(|e| e.to_string())?;
            Ok((flujo, d.name().unwrap_or_default(), frecuencia))
        })();
        match abierto {
            Ok((flujo, n, f)) => {
                let _ = listo_tx.send(Ok((n, f)));
                // El flujo vive mientras viva el proceso.
                loop { std::thread::park(); let _ = &flujo; }
            }
            Err(e) => { let _ = listo_tx.send(Err(e)); }
        }
    })?;
    let (dispositivo, frecuencia) = listo_rx.recv().map_err(|e| e.to_string())??;
    registrar(t0, &format!("micro: {dispositivo} a {frecuencia} Hz → Opus {FRECUENCIA} Hz mono, 20 ms (VoIP)"));
    // El hilo del codificador.
    let m = micro.clone();
    std::thread::Builder::new().name("micro-opus".into()).spawn(move || {
        let Ok(mut opus) = opus::Encoder::new(FRECUENCIA, opus::Channels::Mono, opus::Application::Voip) else { return };
        let mut remuestreo = Remuestreo::nuevo(frecuencia);
        let (mut pendiente, mut salida) = (Vec::<f32>::new(), vec![0u8; 1500]);
        for bloque in rx {
            remuestreo.procesar(&bloque, &mut pendiente);
            while pendiente.len() >= MUESTRAS_TRAMA {
                let trama: Vec<f32> = pendiente.drain(..MUESTRAS_TRAMA).collect();
                m.medir(&trama);
                if let Ok(n) = opus.encode_float(&trama, &mut salida) { m.encolar(salida[..n].to_vec()) }
            }
        }
    })?;
    Ok(micro)
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn remuestreo_a_24k() {
        for origen in [48_000u32, 44_100, 16_000, 24_000] {
            let mut r = Remuestreo::nuevo(origen);
            let mut salida = Vec::new();
            // Un segundo en bloques de 10 ms: sale (casi) un segundo a 24 kHz.
            for _ in 0..100 { r.procesar(&vec![0.5; (origen / 100) as usize], &mut salida) }
            assert!((salida.len() as i64 - 24_000).abs() <= 2, "{origen}: {}", salida.len());
            assert!(salida.iter().all(|&x| (x - 0.5).abs() < 1e-6 || x == 0.0));
        }
    }

    #[test]
    fn cola_corta_y_nivel() {
        let m = Micro::default();
        for i in 0..8u8 { m.encolar(vec![i]) }
        assert_eq!((m.siguiente(), m.tiradas.load(Ordering::Relaxed)), (Some(vec![3]), 3));
        m.medir(&[0.5; 480]);
        let db = m.nivel_dbfs().unwrap();
        assert!((db - -6.02).abs() < 0.1, "{db}");
        assert_eq!(m.nivel_dbfs(), None);
    }

    #[test]
    fn opus_de_voz_a_24k() {
        let mut e = opus::Encoder::new(FRECUENCIA, opus::Channels::Mono, opus::Application::Voip).unwrap();
        let tono: Vec<f32> = (0..MUESTRAS_TRAMA).map(|i| (i as f32 * 0.1).sin() * 0.3).collect();
        let mut b = [0u8; 1500];
        let n = e.encode_float(&tono, &mut b).unwrap();
        assert!(n > 3 && n < 400);
        let mut d = opus::Decoder::new(FRECUENCIA, opus::Channels::Mono).unwrap();
        let mut pcm = [0f32; MUESTRAS_TRAMA];
        assert_eq!(d.decode_float(&b[..n], &mut pcm, false).unwrap(), MUESTRAS_TRAMA);
    }
}
