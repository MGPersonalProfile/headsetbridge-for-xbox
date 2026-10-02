//! Registro: por la salida estándar (si la hay) y a
//! %LOCALAPPDATA%\XboxSoloAudio\rust.log, porque el exe del producto no tiene
//! ventana de consola. Rota a rust.log.1 al pasar de 2 MiB. Nunca lleva tokens.
//!
//! El disco lo toca un hilo propio: quien
//! registra —el hilo único de tokio con la sesión viva, el de audio— sólo
//! encola la línea; un `create`/`write` lento (antivirus, NTFS) no lo para.

use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;
use std::time::Instant;

const MAXIMO: u64 = 2 * 1024 * 1024;

enum Orden { Linea(String), Vaciar(Sender<()>) }

static ESCRITOR: OnceLock<Sender<Orden>> = OnceLock::new();
/// líneas encoladas que el hilo del disco aún no ha escrito (el
/// canal no tiene cota: si el disco se atasca, crece aquí).
static EN_COLA: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn en_cola() -> u64 { EN_COLA.load(std::sync::atomic::Ordering::Relaxed) }

fn ruta() -> PathBuf {
    PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default()).join("XboxSoloAudio").join("rust.log")
}

fn escritor() -> &'static Sender<Orden> {
    ESCRITOR.get_or_init(|| {
        let (tx, rx) = channel::<Orden>();
        std::thread::Builder::new().name("registro".into()).spawn(move || {
            let ruta = ruta();
            if let Some(carpeta) = ruta.parent() { let _ = std::fs::create_dir_all(carpeta); }
            let abrir = || std::fs::OpenOptions::new().create(true).append(true).open(&ruta).ok();
            let mut fichero = abrir();
            let mut largo = std::fs::metadata(&ruta).map(|m| m.len()).unwrap_or(0);
            for orden in rx {
                match orden {
                    Orden::Linea(l) => {
                        EN_COLA.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                        if largo > MAXIMO {
                            drop(fichero.take()); // Windows no renombra un fichero abierto.
                            let _ = std::fs::rename(&ruta, ruta.with_extension("log.1"));
                            fichero = abrir();
                            largo = 0;
                        }
                        if fichero.is_none() { fichero = abrir() }
                        if let Some(f) = fichero.as_mut() {
                            if writeln!(f, "{l}").is_ok() { largo += l.len() as u64 + 1 } else { fichero = None }
                        }
                    }
                    Orden::Vaciar(listo) => { let _ = listo.send(()); }
                }
            }
        }).expect("hilo del registro");
        tx
    })
}

pub fn registrar(t0: Instant, texto: &str) {
    let linea = format!("[{:>6} ms] {texto}", t0.elapsed().as_millis());
    // Sin consola (subsistema Windows), la salida estándar descarta sin fallar.
    println!("{linea}");
    EN_COLA.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if escritor().send(Orden::Linea(linea)).is_err() { EN_COLA.fetch_sub(1, std::sync::atomic::Ordering::Relaxed); }
}

/// Espera (hasta 2 s) a que lo encolado esté en el disco: antes de salir del proceso.
pub fn vaciar() {
    let (tx, rx) = channel();
    if escritor().send(Orden::Vaciar(tx)).is_ok() { let _ = rx.recv_timeout(std::time::Duration::from_secs(2)); }
}

#[cfg(test)]
mod pruebas {
    #[test]
    fn registrar_no_espera_al_disco_y_vaciar_lo_escribe() {
        let carpeta = std::env::temp_dir().join(format!("registro-{}", std::process::id()));
        std::env::set_var("LOCALAPPDATA", &carpeta);
        let t0 = std::time::Instant::now();
        for i in 0..1000 { super::registrar(t0, &format!("línea {i}")) }
        super::vaciar();
        let texto = std::fs::read_to_string(carpeta.join("XboxSoloAudio").join("rust.log")).unwrap();
        assert_eq!(texto.lines().count(), 1000);
        assert!(texto.lines().last().unwrap().ends_with("línea 999"));
        let _ = std::fs::remove_dir_all(carpeta);
    }
}
