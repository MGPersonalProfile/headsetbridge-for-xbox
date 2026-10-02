//! Lo que comparten las dos rutas (WebRTC y nativa): la medida del audio
//! recibido y cómo termina una sesión. Aparte de `sesion.rs` para que el exe
//! sin WebRTC no la necesite.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

fn registro(t0: Instant, texto: &str) { crate::relevo::reg(t0, texto) }

/// la memoria del proceso para el resumen de
/// cada 10 s: (privada, conjunto de trabajo) en bytes.
#[cfg(windows)]
pub fn memoria() -> Option<(u64, u64)> {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
    use windows::Win32::System::Threading::GetCurrentProcess;
    let mut c = PROCESS_MEMORY_COUNTERS_EX { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
    unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS, c.cb) }.ok()?;
    Some((c.PrivateUsage as u64, c.WorkingSetSize as u64))
}

/// Fuera de Windows (pruebas): el residente de /proc como las dos.
#[cfg(not(windows))]
pub fn memoria() -> Option<(u64, u64)> {
    let s = std::fs::read_to_string("/proc/self/statm").ok()?;
    let mut c = s.split_whitespace().map(|x| x.parse::<u64>().ok());
    let (_, residente) = (c.next()??, c.next()??);
    Some((residente * 4096, residente * 4096))
}

/// Lo que se cuenta del audio recibido; se lee cada 10 s.
#[derive(Default)]
pub struct Medida {
    pub paquetes: AtomicU64,
    pub bytes: AtomicU64,
    pub celt_fb_estereo: AtomicU64,
    /// Paquetes de otra fuente de audio (otro SSRC), que no van a la reserva.
    pub otros: AtomicU64,
    /// Copias redundantes (RED) recibidas.
    pub redundantes: AtomicU64,
    /// ms (desde t0) del último /play; 0 = ya llegó su primer audio.
    pub play_ms: AtomicU64,
}

impl Medida {
    /// Se llama justo antes de /play.
    pub fn marcar_play(&self, t0: Instant) { self.play_ms.store(t0.elapsed().as_millis().max(1) as u64, Ordering::Relaxed) }
    /// Se llama con cada paquete de audio: el primero tras /play registra cuánto tardó.
    pub fn audio(&self, t0: Instant) {
        let play = self.play_ms.swap(0, Ordering::Relaxed);
        if play > 0 { registro(t0, &format!("primer audio a {} ms del /play", t0.elapsed().as_millis() as u64 - play)) }
    }
}

/// Cómo terminó una sesión: cumplió su tiempo, o se cortó y hay que reintentar.
#[derive(Debug)]
pub enum Fin {
    Cumplido,
    Corte(String),
}

