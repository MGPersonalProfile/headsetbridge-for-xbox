//! Canal de depuración en vivo (`--depurar=PUERTO`): una conexión TCP SÓLO en
//! 127.0.0.1 que recibe órdenes de una línea y las aplica a la sesión en curso,
//! para experimentar sin recompilar. Lo usa el MCP de `mcp-rust/`.
//!
//!   control <json>   message <json>   input <hex>
//!   mando on|off     tramas on|off    pulso     parar
//!
//! Ruta nativa, con la cabecera DCT y la secuencia nuestras:
//!
//!   crudo <canal> <hex>              un MuxDCTChannel Data con esa carga
//!   trama <canal> <subtipo> [hex]    un MuxDCTChannel Frame (3 = FrameAck)
//!   mensaje <ruta> <json>            un mensaje de Messaging (pide acuse)
//!   boton <arriba|abajo|izquierda|derecha|a|b> [ms]   pulsa y suelta a los ms
//!                                    (100: medir la latencia con el
//!                                    «tic» de navegación y `--volcar-wav`)
//!   periodico <s> <orden…>           repetirla cada s segundos (`periodico 0` para)
//!
//! `<canal>`: control, qos, video, audio, messaging, chataudio, input,
//! inputfeedback, o el SSRC. Lo que conteste la consola en ese canal (los 20
//! paquetes siguientes) sale en el registro.
//!
//! Responde una línea: `ok`, o `error: …`.

use std::sync::OnceLock;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, Mutex};

#[derive(Debug, Clone)]
pub enum Orden {
    Crudo { ssrc: u32, carga: Vec<u8> },
    Trama { ssrc: u32, subtipo: u32, carga: Vec<u8> },
    Mensaje { ruta: String, valor: Vec<u8> },
    Control(Vec<u8>),
    Message(Vec<u8>),
    Input(Vec<u8>),
    Mando(bool),
    Tramas(bool),
    Pulso,
    Boton { cual: crate::nano::protocolo::Boton, ms: u64 },
    Parar,
}

static RECEPTOR: OnceLock<Mutex<mpsc::UnboundedReceiver<Orden>>> = OnceLock::new();

/// La siguiente orden; si no hay canal de depuración, no vuelve nunca.
pub async fn siguiente() -> Orden {
    match RECEPTOR.get() {
        Some(r) => match r.lock().await.recv().await {
            Some(o) => o,
            None => std::future::pending().await,
        },
        None => std::future::pending().await,
    }
}

fn hex(texto: &str) -> Result<Vec<u8>, String> {
    let limpio: String = texto.chars().filter(|c| !c.is_whitespace()).collect();
    if limpio.len() % 2 != 0 { return Err("hex de longitud impar".into()) }
    (0..limpio.len()).step_by(2).map(|i| u8::from_str_radix(&limpio[i..i + 2], 16).map_err(|e| e.to_string())).collect()
}

fn interpretar(linea: &str) -> Result<Orden, String> {
    let (orden, resto) = linea.trim().split_once(' ').unwrap_or((linea.trim(), ""));
    let resto = resto.trim();
    Ok(match (orden, resto) {
        ("control", j) if !j.is_empty() => Orden::Control(j.as_bytes().to_vec()),
        ("message", j) if !j.is_empty() => Orden::Message(j.as_bytes().to_vec()),
        ("input", h) if !h.is_empty() => Orden::Input(hex(h)?),
        ("mando", "on") => Orden::Mando(true),
        ("mando", "off") => Orden::Mando(false),
        ("tramas", "on") => Orden::Tramas(true),
        ("tramas", "off") => Orden::Tramas(false),
        ("crudo", r) => {
            let (c, h) = r.split_once(' ').unwrap_or((r, ""));
            Orden::Crudo { ssrc: canal(c)?, carga: hex(h)? }
        }
        ("trama", r) => {
            let mut p = r.splitn(3, ' ');
            let (c, t, h) = (p.next().unwrap_or(""), p.next().unwrap_or(""), p.next().unwrap_or(""));
            Orden::Trama { ssrc: canal(c)?, subtipo: t.parse().map_err(|_| format!("subtipo no numérico: {t}"))?, carga: hex(h)? }
        }
        ("mensaje", r) if r.starts_with('/') => {
            let (ruta, valor) = r.split_once(' ').unwrap_or((r, ""));
            Orden::Mensaje { ruta: ruta.to_owned(), valor: valor.trim().as_bytes().to_vec() }
        }
        ("pulso", _) => Orden::Pulso,
        ("boton", r) => {
            let (b, ms) = r.split_once(' ').unwrap_or((r, ""));
            let cual = crate::nano::protocolo::Boton::de(b).ok_or_else(|| format!("botón desconocido: {b} (arriba, abajo, izquierda, derecha, a, b)"))?;
            let ms = if ms.trim().is_empty() { 100 } else { ms.trim().parse().map_err(|_| format!("ms no numéricos: {ms}"))? };
            Orden::Boton { cual, ms }
        }
        ("parar", _) => Orden::Parar,
        _ => return Err(format!("orden desconocida: {linea}")),
    })
}

fn canal(nombre: &str) -> Result<u32, String> {
    crate::nano::protocolo::ssrc_de(nombre).ok_or_else(|| format!("canal desconocido: {nombre}"))
}

/// `periodico <s> <orden…>`: (segundos, orden); `periodico 0` = (0, None).
fn periodico(resto: &str) -> Result<(u64, Option<Orden>), String> {
    let (s, orden) = resto.trim().split_once(' ').unwrap_or((resto.trim(), ""));
    let s: u64 = s.parse().map_err(|_| format!("segundos no numéricos: {s}"))?;
    if s == 0 { return Ok((0, None)) }
    match interpretar(orden)? {
        // La parada es del programa, no algo que repetir.
        Orden::Parar => Err("parar no se repite".into()),
        o => Ok((s, Some(o))),
    }
}

/// Abre el puerto y deja las órdenes en la cola que lee `sesion`.
/// `parar` es la parada del programa (la misma que pide el segundo clic por la
/// tubería): la orden `parar` va ahí y no a la sesión, que en un relevo sólo
/// terminaba la sesión en curso y el relevo abría la siguiente.
pub async fn escuchar(puerto: u16, t0: std::time::Instant, parar: std::sync::Arc<tokio::sync::Notify>) -> std::io::Result<()> {
    let (tx, rx) = mpsc::unbounded_channel::<Orden>();
    let _ = RECEPTOR.set(Mutex::new(rx));
    let repetida: std::sync::Arc<std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>> = Default::default();
    let oyente = tokio::net::TcpListener::bind(("127.0.0.1", puerto)).await?;
    crate::registro::registrar(t0, &format!("DEPURACIÓN: órdenes en 127.0.0.1:{puerto}"));
    tokio::spawn(async move {
        while let Ok((conexion, _)) = oyente.accept().await {
            let (tx, repetida, parar) = (tx.clone(), repetida.clone(), parar.clone());
            tokio::spawn(async move {
                let (lectura, mut escritura) = conexion.into_split();
                let mut lineas = BufReader::new(lectura).lines();
                while let Ok(Some(linea)) = lineas.next_line().await {
                    if linea.trim().is_empty() { continue }
                    if let Some(resto) = linea.trim().strip_prefix("periodico ") {
                        let respuesta = match periodico(resto) {
                            Ok((s, orden)) => {
                                crate::registro::registrar(t0, &format!("DEPURACIÓN ← {}", linea.chars().take(200).collect::<String>()));
                                if let Some(h) = repetida.lock().ok().and_then(|mut r| r.take()) { h.abort() }
                                if let Some(o) = orden {
                                    let tx = tx.clone();
                                    let h = tokio::spawn(async move {
                                        let mut cada = crate::red::intervalo(std::time::Duration::from_secs(s));
                                        loop { cada.tick().await; if tx.send(o.clone()).is_err() { break } }
                                    });
                                    if let Ok(mut r) = repetida.lock() { *r = Some(h) }
                                }
                                "ok\n".to_owned()
                            }
                            Err(e) => format!("error: {e}\n"),
                        };
                        if escritura.write_all(respuesta.as_bytes()).await.is_err() { break }
                        continue;
                    }
                    let respuesta = match interpretar(&linea) {
                        Ok(Orden::Parar) => {
                            crate::registro::registrar(t0, "DEPURACIÓN ← parar: parada del programa (como el segundo clic)");
                            parar.notify_one();
                            "ok\n".to_owned()
                        }
                        Ok(orden) => {
                            crate::registro::registrar(t0, &format!("DEPURACIÓN ← {}", linea.chars().take(200).collect::<String>()));
                            if tx.send(orden).is_ok() { "ok\n".to_owned() } else { "error: sin sesión\n".to_owned() }
                        }
                        Err(e) => format!("error: {e}\n"),
                    };
                    if escritura.write_all(respuesta.as_bytes()).await.is_err() { break }
                }
            });
        }
    });
    Ok(())
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn ordenes_de_la_ruta_nativa() {
        assert!(matches!(interpretar("crudo input 0a0b").unwrap(), Orden::Crudo { ssrc: 1030, carga } if carga == [10, 11]));
        assert!(matches!(interpretar("trama inputfeedback 3 01000000").unwrap(), Orden::Trama { ssrc: 1031, subtipo: 3, carga } if carga == [1, 0, 0, 0]));
        assert!(matches!(interpretar("trama 1026 3").unwrap(), Orden::Trama { ssrc: 1026, subtipo: 3, carga } if carga.is_empty()));
        assert!(matches!(interpretar(r#"mensaje /streaming/x {"a":1}"#).unwrap(), Orden::Mensaje { ruta, valor } if ruta == "/streaming/x" && valor == br#"{"a":1}"#));
        assert!(interpretar("crudo nada 00").is_err());
        assert!(matches!(periodico("10 trama input 3 01000000").unwrap(), (10, Some(Orden::Trama { .. }))));
        assert!(matches!(periodico("0").unwrap(), (0, None)));
        assert!(matches!(interpretar("boton arriba").unwrap(), Orden::Boton { cual: crate::nano::protocolo::Boton::Arriba, ms: 100 }));
        assert!(matches!(interpretar("boton a 250").unwrap(), Orden::Boton { cual: crate::nano::protocolo::Boton::A, ms: 250 }));
        assert!(interpretar("boton x").is_err());
    }
}
