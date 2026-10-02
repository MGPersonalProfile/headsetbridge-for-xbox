//! `--volcar-wav=RUTA`: lo que sale por la tarjeta (48 kHz
//! estéreo, s16) a un WAV, para comprobar con correlación si la música sigue
//! justo donde se cortó en cada relevo. Cada relevo deja en `rust.log` la
//! muestra del WAV en que ocurre (`marca()`).
//!
//! El hilo de audio sólo convierte y encola (`try_send`: nunca espera); el
//! disco lo toca un hilo propio, que reescribe la cabecera cada segundo para
//! que el fichero valga aunque el proceso muera.

use std::io::{Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::OnceLock;

const FRECUENCIA: u32 = 48_000;
const CANALES: u16 = 2;

static ESCRITOR: OnceLock<SyncSender<Vec<i16>>> = OnceLock::new();
/// Muestras por canal entregadas al escritor (la posición en el WAV).
static MUESTRAS: AtomicU64 = AtomicU64::new(0);
static PERDIDAS: AtomicU64 = AtomicU64::new(0);

fn cabecera(datos: u32) -> Vec<u8> {
    let mut c = Vec::with_capacity(44);
    let bloque = CANALES * 2;
    c.extend_from_slice(b"RIFF");
    c.extend_from_slice(&(36 + datos).to_le_bytes());
    c.extend_from_slice(b"WAVEfmt ");
    c.extend_from_slice(&16u32.to_le_bytes());
    c.extend_from_slice(&1u16.to_le_bytes());
    c.extend_from_slice(&CANALES.to_le_bytes());
    c.extend_from_slice(&FRECUENCIA.to_le_bytes());
    c.extend_from_slice(&(FRECUENCIA * u32::from(bloque)).to_le_bytes());
    c.extend_from_slice(&bloque.to_le_bytes());
    c.extend_from_slice(&16u16.to_le_bytes());
    c.extend_from_slice(b"data");
    c.extend_from_slice(&datos.to_le_bytes());
    c
}

pub fn abrir(ruta: &str) -> std::io::Result<()> {
    let mut f = std::fs::File::create(ruta)?;
    f.write_all(&cabecera(0))?;
    let (tx, rx) = sync_channel::<Vec<i16>>(256);
    std::thread::Builder::new().name("volcado-wav".into()).spawn(move || {
        let (mut bytes, mut ultima) = (0u64, std::time::Instant::now());
        for bloque in rx {
            let b: Vec<u8> = bloque.iter().flat_map(|m| m.to_le_bytes()).collect();
            if f.write_all(&b).is_err() { return }
            bytes += b.len() as u64;
            if ultima.elapsed().as_secs() >= 1 {
                ultima = std::time::Instant::now();
                let datos = bytes.min(u64::from(u32::MAX - 36)) as u32;
                let _ = f.seek(SeekFrom::Start(0)).and_then(|_| f.write_all(&cabecera(datos))).and_then(|_| f.seek(SeekFrom::End(0)));
            }
        }
    })?;
    let _ = ESCRITOR.set(tx);
    Ok(())
}

/// Desde el hilo de audio: lo que acaba de ir a la tarjeta (f32 intercalado).
pub fn escribir(datos: &[f32]) {
    let Some(tx) = ESCRITOR.get() else { return };
    let bloque: Vec<i16> = datos.iter().map(|&x| (x.clamp(-1.0, 1.0) * 32767.0) as i16).collect();
    MUESTRAS.fetch_add((datos.len() / usize::from(CANALES)) as u64, Ordering::Relaxed);
    if tx.try_send(bloque).is_err() { PERDIDAS.fetch_add(1, Ordering::Relaxed); }
}

/// « · WAV muestra N (S s)» para la línea del relevo; vacío sin volcado.
pub fn marca() -> String {
    if ESCRITOR.get().is_none() { return String::new() }
    let n = MUESTRAS.load(Ordering::Relaxed);
    let p = PERDIDAS.load(Ordering::Relaxed);
    format!(" · WAV muestra {n} ({:.3} s){}", n as f64 / f64::from(FRECUENCIA), if p > 0 { format!(", {p} bloques sin volcar") } else { String::new() })
}

#[cfg(test)]
mod pruebas {
    #[test]
    fn cabecera_wav() {
        let c = super::cabecera(1000);
        assert_eq!((c.len(), &c[0..4], &c[8..16], &c[36..40]), (44, &b"RIFF"[..], &b"WAVEfmt "[..], &b"data"[..]));
        assert_eq!(u32::from_le_bytes(c[24..28].try_into().unwrap()), 48_000);
        assert_eq!(u32::from_le_bytes(c[40..44].try_into().unwrap()), 1000);
    }
}
