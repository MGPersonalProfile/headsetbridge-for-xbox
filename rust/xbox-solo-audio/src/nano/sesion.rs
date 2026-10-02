//! Una sesión por la ruta Nano V3 nativa (`--nativo`): /play con V3 nativo,
//! /configuration (clave SRTP), /ice con el JSON de candidatos, ICE activo
//! (el cliente comprueba primero; la consola no lo hace), y el protocolo de
//! `protocolo.rs` sobre un socket UDP propio con SRTP AES-128-GCM. El audio
//! va a la misma `Reserva` que la ruta WebRTC.

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::net::UdpSocket;

use super::protocolo::{Evento, Opciones, Protocolo, Salida};
use super::srtp::{cabecera_rtp, leer_cabecera, Srtp};
use super::caja::{Caja, Suceso};
use super::stun;
use crate::audio::ReservaCompartida;
use crate::medida::{Fin, Medida};
use crate::xhome::XHome;

type Resultado<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Los candidatos de la consola del intento anterior: en un re-enganche, el
/// `GET /ice` puede devolver aún los del transporte viejo.
static ULTIMO_ICE: std::sync::Mutex<Option<Value>> = std::sync::Mutex::new(None);

/// El id de sesión y la clave SRTP del intento anterior (para registrar si se repiten).
static ULTIMA_SESION: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
static ULTIMA_CLAVE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Prioridad y desempate de las comprobaciones (los de greenlight-native).
const PRIORIDAD: u32 = 2_130_705_919;
const DESEMPATE: u64 = 0x89ec_5f41_7241_e2d2;
/// La consola manda ~50 tramas de audio por segundo también en silencio.
const PLAZO_DATOS: Duration = Duration::from_secs(5);

use crate::relevo::reg;

#[derive(Clone)]
pub struct Ajustes {
    pub protocolo: Opciones,
    /// Cada cuánto se manda una trama de entrada (greenlight-native: 12 ms).
    pub entrada_cada: Duration,
    /// Silencio por ChatAudio cada 20 ms (experimento del reto).
    pub chat_silencio: bool,
    /// `--micro`: tramas Opus del micro del PC por ChatAudio cada
    /// 20 ms (silencio cuando la cola está vacía).
    pub micro: Option<Arc<crate::micro::Micro>>,
    /// tras el `/play`, `/state`,
    /// `/configuration` y `/ice` a la vez, sondeados cada 50/50/25 ms.
    pub arranque_paralelo: bool,
    /// `--play-anticipado=MS`: Play por la nube a MS ms del `/play` de cada relevo.
    pub play_anticipado: Option<u64>,
    /// Número de la sesión en un relevo (`--relevo`); 0 fuera de él.
    pub numero: u64,
}

impl Default for Ajustes {
    fn default() -> Self { Self { protocolo: Opciones::default(), entrada_cada: Duration::from_millis(12), chat_silencio: false, micro: None, arranque_paralelo: false, play_anticipado: None, numero: 0 } }
}

fn aleatorio(n: usize) -> String {
    let mut s = String::new();
    while s.len() < n { s.push_str(&uuid::Uuid::new_v4().simple().to_string()) }
    s.truncate(n);
    s
}

fn transaccion() -> [u8; 12] { uuid::Uuid::new_v4().as_bytes()[..12].try_into().unwrap() }

/// La IPv4 de la interfaz por la que sale el tráfico (sin mandar nada: un
/// `connect` UDP sólo elige ruta).
fn ip_local() -> Resultado<std::net::IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0")?;
    s.connect("8.8.8.8:53")?;
    Ok(s.local_addr()?.ip())
}

/// El JSON de candidatos de greenlight-native (`packages/xcloud/src/index.ts`).
fn candidatos(direccion: SocketAddr, usuario: &str, clave: &str) -> Value {
    json!({
        "Full": "1", "PacingMs": "50", "Version": "1",
        "Candidates": {
            "0": { "transportAddress": direccion.to_string(), "baseAddress": direccion.to_string(), "serverAddress": "",
                "ipv6": "0", "type": "3", "addressType": 0, "priority": PRIORIDAD.to_string(), "foundation": "1310976135", "transport": "udp" },
            "count": 1,
        },
        "Username": usuario, "Password": clave,
    })
}

/// Candidatos IPv4 de la consola (sólo IPv4: los IPv6 con «%interfaz» fallan en Windows).
fn remotos(ice: &Value) -> Vec<SocketAddr> {
    ice["Candidates"].as_object().map(|c| c.iter()
        .filter(|(k, v)| k.as_str() != "count" && v["ipv6"].as_str() != Some("1"))
        .filter_map(|(_, v)| v["transportAddress"].as_str()?.parse().ok())
        .filter(|a: &SocketAddr| a.is_ipv4())
        .collect()).unwrap_or_default()
}

fn hex(b: &[u8]) -> String { b.iter().map(|x| format!("{x:02x}")).collect() }

/// Cifra y manda lo que devuelve el protocolo; la secuencia RTP es una para todos los SSRC.
struct Emisor { srtp: Srtp, secuencia: u64, destino: SocketAddr, caja: Caja }

impl Emisor {
    async fn mandar(&mut self, socket: &UdpSocket, s: Salida) {
        let cabecera = cabecera_rtp(s.pt, self.secuencia as u16, s.ssrc);
        let paquete = self.srtp.cifrar(&cabecera, &s.carga, (self.secuencia >> 16) as u32);
        self.secuencia += 1;
        let ok = socket.send_to(&paquete, self.destino).await.is_ok();
        self.caja.anotar(Suceso::Mandado(s.pt, s.ssrc, ok));
    }
}

/// ROC del servidor (RFC 3711 §3.3.1): la secuencia RTP de la consola es una
/// sola para todos sus SSRC y da la vuelta a los ~22 min de audio.
#[derive(Default)]
struct Roc { roc: u32, ultima: Option<u16> }

impl Roc {
    fn estimar(&self, s: u16) -> u32 {
        match self.ultima {
            Some(l) if l > s && l - s > 0x8000 => self.roc.wrapping_add(1),
            Some(l) if s > l && s - l > 0x8000 => self.roc.wrapping_sub(1),
            _ => self.roc,
        }
    }
    fn aceptar(&mut self, s: u16, roc: u32) {
        if roc != self.roc && roc == self.roc.wrapping_add(1) { self.roc = roc; self.ultima = Some(s) }
        else if roc == self.roc && self.ultima.is_none_or(|l| (s.wrapping_sub(l) as i16) > 0) { self.ultima = Some(s) }
    }
}

pub async fn sesion(mut xhome: XHome, consola: &str, hasta: tokio::time::Instant, t0: Instant, medida: Arc<Medida>, reserva: Option<ReservaCompartida>, parar: Arc<tokio::sync::Notify>, ajustes: &Ajustes) -> Resultado<Fin> {
    let socket = crate::red::socket_udp().await?;
    let local = SocketAddr::new(ip_local()?, socket.local_addr()?.port());
    let (usuario, clave) = (aleatorio(8), aleatorio(24));

    medida.marcar_play(t0);
    let play = Instant::now();
    crate::relevo::hito(ajustes.numero, |h| &mut h.play);
    if let (Some(ms), true) = (ajustes.play_anticipado, ajustes.numero > 1) { crate::relevo::play_anticipado(ajustes.numero, t0, ms) }
    xhome.arrancar_nativo(consola).await?;
    // Huella del id: si un /play nativo tras una sesión sin cerrar devuelve el
    // mismo id, se re-engancha a ella (medido el 01-10, «sesión zombi»).
    let id = xhome.sesion.clone().unwrap_or_default();
    let anterior = ULTIMA_SESION.lock().ok().and_then(|mut u| u.replace(id.clone()));
    reg(t0, &format!("sesión nativa creada: id {}{}", crate::xhome::huella(&id),
        match anterior { Some(a) if a == id => " (LA MISMA que el intento anterior: re-enganche)", Some(_) => " (nueva)", None => "" }));
    let resultado = conectar_y_sonar(&mut xhome, socket, local, &usuario, &clave, hasta, t0, medida, reserva, parar, ajustes, play).await;
    // Cerrar siempre: una sesión que muere abierta bloquea la siguiente (409) hasta que caduca.
    if crate::xhome::reenganchar() && !matches!(resultado, Ok(Fin::Cumplido)) {
        reg(t0, "DIAGNÓSTICO: sesión sin DELETE, para re-engancharse");
    } else if let Err(e) = xhome.parar().await { reg(t0, &format!("no se pudo cerrar la sesión en la consola: {e}")) }
    resultado
}

/// el arranque tras el `/play`, con todo a la vez en vez de en
/// serie (~0,6 s del hueco eran nuestros). `/state` cada
/// 50 ms (con `/connect` en ReadyToConnect), `GET /configuration` cada 50 ms y
/// `POST /ice` con nuestros candidatos seguido de `GET /ice` cada 25 ms. Si el
/// POST falla antes de Provisioned, se repite al llegar. En un re-enganche el
/// `GET /ice` puede dar los candidatos del transporte viejo: valen los
/// distintos de los del intento anterior, o los mismos si 1 s después de
/// Provisioned no han cambiado.
async fn arranque_paralelo(xhome: &XHome, cand: &Value, t0: Instant, n: u64, desde: impl Fn() -> u128 + Copy) -> Resultado<(Value, Value)> {
    let (tx, rx) = tokio::sync::watch::channel::<Option<Instant>>(None);
    let estado = async {
        xhome.esperar_consola_cada(Duration::from_millis(50), |e| reg(t0, &format!("consola: {e} (+{} ms del /play)", desde()))).await?;
        crate::relevo::hito(n, |h| &mut h.provisioned);
        let _ = tx.send(Some(Instant::now()));
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    };
    let configuracion = async {
        let fin = Instant::now() + Duration::from_secs(30);
        let mut fallos = 0u32;
        loop {
            match xhome.get_v4("/configuration").await {
                Ok(c) if !c["serverDetails"].is_null() => {
                    crate::relevo::hito(n, |h| &mut h.configuracion);
                    reg(t0, &format!("arranque: /configuration (+{} ms del /play{})", desde(), if fallos > 0 { format!(", tras {fallos} respuestas sin ella") } else { String::new() }));
                    return Ok::<_, Box<dyn std::error::Error + Send + Sync>>(c);
                }
                // Antes de Provisioned puede contestar sin la configuración o con error: se insiste.
                Ok(_) | Err(_) if Instant::now() < fin => fallos += 1,
                Ok(_) => return Err("/configuration: sin serverDetails en 30 s".into()),
                Err(e) => return Err(e),
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    let ice = async {
        let mut rx = rx.clone();
        match xhome.ice_post(cand).await {
            Ok(()) => reg(t0, &format!("arranque: POST /ice (+{} ms del /play)", desde())),
            Err(e) => {
                reg(t0, &format!("arranque: POST /ice antes de Provisioned falló ({e}); se repite al llegar"));
                rx.wait_for(|p| p.is_some()).await.map_err(|_| "sin Provisioned para repetir el POST /ice")?;
                xhome.ice_post(cand).await?;
                reg(t0, &format!("arranque: POST /ice repetido tras Provisioned (+{} ms del /play)", desde()));
            }
        }
        let previo = ULTIMO_ICE.lock().ok().and_then(|u| u.clone());
        let fin = Instant::now() + Duration::from_secs(30);
        let mut viejos = 0u32;
        loop {
            if let Ok(r) = xhome.get_v4("/ice").await {
                if let Some(c) = r["candidates"].as_str().and_then(|c| serde_json::from_str::<Value>(c).ok()) {
                    // Con el Provisioned de verdad, lo nuevo llega enseguida;
                    // los mismos de antes sólo valen si 1 s después siguen sin cambiar.
                    let tarde = rx.borrow().is_some_and(|p| p.elapsed() > Duration::from_millis(1000));
                    if previo.as_ref() != Some(&c) || tarde {
                        crate::relevo::hito(n, |h| &mut h.ice);
                        reg(t0, &format!("arranque: candidatos de la consola (+{} ms del /play){}", desde(),
                            if previo.as_ref() == Some(&c) { " (los mismos del intento anterior: 1 s tras Provisioned sin cambiar)".to_owned() } else if viejos > 0 { format!(" (antes, {viejos} veces los del transporte viejo)") } else { String::new() }));
                        return Ok::<_, Box<dyn std::error::Error + Send + Sync>>(c);
                    }
                    viejos += 1;
                }
            }
            if Instant::now() > fin { return Err("/ice: sin candidatos nuevos en 30 s".into()) }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    };
    let ((), c, i) = tokio::try_join!(estado, configuracion, ice)?;
    Ok((c, i))
}

#[allow(clippy::too_many_arguments)]
async fn conectar_y_sonar(xhome: &mut XHome, socket: UdpSocket, local: SocketAddr, usuario: &str, clave: &str, hasta: tokio::time::Instant, t0: Instant, medida: Arc<Medida>, reserva: Option<ReservaCompartida>, parar: Arc<tokio::sync::Notify>, ajustes: &Ajustes, play: Instant) -> Resultado<Fin> {
    let desde = move || play.elapsed().as_millis();
    let n = ajustes.numero;
    let (config, ice) = if ajustes.arranque_paralelo {
        arranque_paralelo(xhome, &candidatos(local, usuario, clave), t0, n, desde).await?
    } else {
        xhome.esperar_consola(|e| reg(t0, &format!("consola: {e} (+{} ms del /play)", desde()))).await?;
        crate::relevo::hito(n, |h| &mut h.provisioned);
        let config = xhome.configuracion_nativa().await?;
        crate::relevo::hito(n, |h| &mut h.configuracion);
        let ice = xhome.intercambiar_ice_nativo(&candidatos(local, usuario, clave)).await?;
        crate::relevo::hito(n, |h| &mut h.ice);
        (config, ice)
    };
    if let Ok(mut u) = ULTIMO_ICE.lock() { *u = Some(ice.clone()) }
    let clave_srtp = config["serverDetails"]["srtp"]["key"].as_str().ok_or("/configuration sin serverDetails.srtp.key")?.to_owned();
    {
        // Lo que dice la consola, sin la clave.
        let mut detalles = config["serverDetails"].clone();
        if let Some(d) = detalles.as_object_mut() { d.remove("srtp"); }
        reg(t0, &format!("configuración: {detalles} · keepAlivePulseInSeconds {} (+{} ms del /play)", config["keepAlivePulseInSeconds"], desde()));
    }
    let anterior = ULTIMA_CLAVE.lock().ok().and_then(|mut u| u.replace(clave_srtp.clone()));
    reg(t0, &format!("clave SRTP: huella {}{}", crate::xhome::huella(&clave_srtp),
        match anterior { Some(a) if a == clave_srtp => " (la misma que el intento anterior)", Some(_) => " (distinta del intento anterior)", None => "" }));
    let srtp = Srtp::nuevo(&clave_srtp)?;

    let (usuario_remoto, clave_remota) = (ice["Username"].as_str().unwrap_or("").to_owned(), ice["Password"].as_str().unwrap_or("").to_owned());
    let remotos = remotos(&ice);
    reg(t0, &format!("candidato local {local}; de la consola (IPv4): {}", remotos.iter().map(|r| r.to_string()).collect::<Vec<_>>().join(", ")));
    if remotos.is_empty() { return Err("la consola no dio candidatos IPv4".into()) }

    // ICE activo: comprobaciones (con USE-CANDIDATE) cada 500 ms a todos los
    // candidatos; se contesta a las de la consola. Como greenlight-native, la
    // conexión vale cuando la consola vuelve a comprobar DESPUÉS de que haya
    // contestado a una nuestra (el par ya está nominado por los dos lados).
    let usuario_check = format!("{usuario_remoto}:{usuario}");
    let mut buf = vec![0u8; 2048];
    // Cada 50 ms (greenlight-native declara `PacingMs: 50`; antes, 500).
    let mut cada = crate::red::intervalo(Duration::from_millis(50));
    let plazo = tokio::time::sleep(Duration::from_secs(15));
    tokio::pin!(plazo);
    let mut enviadas: Vec<[u8; 12]> = Vec::new();
    let mut contestada: Option<SocketAddr> = None;
    let mut tolerancia = crate::red::Tolerancia::default();
    // Lo que no es STUN y llega antes de dar la conexión por buena (la consola
    // ya se cree conectada y manda sus sondeos de MTU, de 1436 B para abajo):
    // se guarda para el protocolo. Tirarlo dejaba el MTU en 1232 (01-10).
    let mut pendientes: Vec<Vec<u8>> = Vec::new();
    let destino = loop {
        tokio::select! {
            _ = &mut plazo => return Err(format!("ICE: sin conexión en 15 s ({})", if contestada.is_some() { "la consola contestó, pero no volvió a comprobar" } else { "la consola no contestó" }).into()),
            _ = parar.notified() => return Ok(Fin::Cumplido),
            _ = cada.tick() => for r in &remotos {
                let t = transaccion();
                let b = stun::comprobacion(&t, &usuario_check, PRIORIDAD, DESEMPATE, &clave_remota);
                let _ = socket.send_to(&b, r).await;
                enviadas.push(t);
            },
            r = socket.recv_from(&mut buf) => {
                let (n, origen) = match r {
                    Ok(r) => { tolerancia.bien(); r }
                    Err(e) => { if let Some(t) = tolerancia.error(e)? { reg(t0, &t) } continue }
                };
                let Some(m) = stun::leer(&buf[..n]) else {
                    if remotos.contains(&origen) && leer_cabecera(&buf[..n]).is_some() {
                        if pendientes.is_empty() { reg(t0, &format!("ICE: la consola ya manda datos desde {origen}: conectado")) }
                        pendientes.push(buf[..n].to_vec());
                        if pendientes.len() >= 4 { break origen }
                    }
                    continue
                };
                if m.tipo == stun::PETICION {
                    let _ = socket.send_to(&stun::respuesta(&m.transaccion, origen, &clave_remota), origen).await;
                    if contestada == Some(origen) || !pendientes.is_empty() { reg(t0, &format!("ICE: conectado con {origen} (+{} ms del /play)", desde())); break origen }
                    reg(t0, &format!("ICE: la consola comprueba desde {origen}"));
                } else if m.tipo == stun::EXITO && enviadas.contains(&m.transaccion) && contestada.is_none() {
                    reg(t0, &format!("ICE: la consola contesta desde {origen}; esperando su comprobación"));
                    contestada = Some(origen);
                }
            }
        }
    };

    crate::relevo::hito(n, |h| &mut h.conectado);
    pendientes.reverse();
    let mut emisor = Emisor { srtp, secuencia: 0, destino, caja: Caja::default() };
    let mut protocolo = Protocolo::nuevo(ajustes.protocolo.clone());
    let mut roc = Roc::default();
    let inicio = Instant::now();
    let us = move || inicio.elapsed().as_micros() as u64;

    let mut sondeo = crate::red::intervalo(Duration::from_millis(10));
    // Cada 50 ms se confirma al núcleo lo recibido si no salió nada con la
    // confirmación dentro, como greenlight-native (`core.ts`,
    // `_sessionConfirmInterval`). Medido: ~7,6/s y no pesa en CPU.
    let mut confirmar = crate::red::intervalo(Duration::from_millis(50));
    let mut confirmaciones = 0u64;
    // El descifrado, en un búfer que se reutiliza.
    let mut claro: Vec<u8> = Vec::with_capacity(2048);
    let mut entrada = crate::red::intervalo(ajustes.entrada_cada);
    // Cada 10 ms se mira cuántas tramas de 20 ms tocan (el envío va por el reloj).
    let mut chat = crate::red::intervalo(Duration::from_millis(10));
    let mut micro_cada = crate::red::intervalo(Duration::from_secs(10));
    micro_cada.tick().await;
    let (mut micro_enviadas, mut micro_silencios) = (0u64, 0u64);
    let (mut chat_desde, mut chat_total): (Option<Instant>, u64) = (None, 0);
    let mut keepalive = crate::red::intervalo(Duration::from_secs(30));
    let mut vigilar = crate::red::intervalo(Duration::from_secs(1));
    let limite = tokio::time::sleep_until(hasta);
    tokio::pin!(limite);
    let mut ultimo_dato = Instant::now();
    let (mut fallos_srtp, mut fallos_avisados) = (0u64, 0u64);
    let keepalive_fallido = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut resumen = crate::red::intervalo(Duration::from_secs(30));
    resumen.tick().await;
    let (mut mando_pc, mut traductor) = (crate::red::intervalo(Duration::from_millis(8)), crate::raton_teclado::Traductor::default());
    let mut resumen_anterior = 0u64;

    let fin = loop {
        let diferida = protocolo.proxima_diferida();
        tokio::select! {
            _ = &mut limite => { reg(t0, "tiempo cumplido"); break Fin::Cumplido }
            _ = parar.notified() => break Fin::Cumplido,
            _ = sondeo.tick(), if protocolo.mtu.is_none() => if let Some(s) = protocolo.sondeo() { emisor.mandar(&socket, s).await },
            _ = confirmar.tick() => {
                if let Some(s) = protocolo.confirmar(us()) { confirmaciones += 1; emisor.mandar(&socket, s).await }
            },
            // El mando en reposo tras un pulso o un `boton`, a su hora (// antes iba en el tic de 50 ms de las confirmaciones).
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(inicio + Duration::from_micros(diferida.unwrap_or(0)))), if diferida.is_some() => {
                for s in protocolo.diferidas(us()) {
                    emisor.mandar(&socket, s).await;
                    match protocolo.soltado.take() {
                        Some(b) => reg(t0, &format!("DEPURACIÓN → boton {b:?} suelto{}", crate::volcado::marca())),
                        None => reg(t0, "pulso: mando en reposo otra vez"),
                    }
                }
            },
            _ = entrada.tick(), if protocolo.entrada_activa => if let Some(s) = protocolo.trama_entrada(us()) { emisor.mandar(&socket, s).await },
            // A18: el ratón y el teclado del PC como mando, a 125 Hz mientras estén capturados.
            _ = mando_pc.tick(), if protocolo.entrada_activa && crate::raton_teclado::ACTIVADO.load(std::sync::atomic::Ordering::Relaxed) => {
                if let Some(e) = crate::raton_teclado::tomar() { if let Some(s) = protocolo.mando_pc(&mut traductor, &e, us()) { emisor.mandar(&socket, s).await } }
            },
            _ = chat.tick(), if ajustes.chat_silencio || ajustes.micro.is_some() => {
                if !protocolo.chat_abierto {
                    if let Some(m) = &ajustes.micro { m.descartar() }
                    (chat_desde, chat_total) = (None, 0);
                    continue
                }
                // Por el reloj y no por el tic (en Windows el temporizador va a
                // ~15,6 ms y salían ~42 tramas/s): las que tocan desde la apertura.
                let desde = *chat_desde.get_or_insert_with(Instant::now);
                let debidas = (desde.elapsed().as_millis() / 20) as u64 + 1;
                for _ in 0..debidas.saturating_sub(chat_total).min(5) {
                    let voz = ajustes.micro.as_ref().and_then(|m| m.siguiente());
                    if voz.is_some() { micro_enviadas += 1 } else if ajustes.micro.is_some() { micro_silencios += 1 }
                    let s = match &voz { Some(v) => protocolo.trama_chat_con(us(), v), None => protocolo.trama_chat(us()) };
                    if let Some(s) = s { emisor.mandar(&socket, s).await }
                    chat_total += 1;
                }
                // Más de 5 de retraso (el hilo se paró): se salta, no se recupera en ráfaga.
                chat_total = chat_total.max(debidas.saturating_sub(5));
            },
            // órdenes en caliente por `--depurar` (MCP xbox-rust, `orden`).
            // En un relevo, sólo la sesión que tiene el audio.
            orden = crate::depurar::siguiente(), if crate::relevo::es_activa(ajustes.numero) => {
                use crate::depurar::Orden;
                // pulsa en el acto; se suelta solo a los ms (rama de arriba).
                if let Orden::Boton { cual, ms } = &orden {
                    let salidas = protocolo.boton(*cual, *ms, us());
                    if salidas.is_empty() { reg(t0, "DEPURACIÓN: boton sin canal Input (modo música o aún sin abrir): no se hace nada") }
                    let n = salidas.len();
                    for s in salidas { emisor.mandar(&socket, s).await }
                    if n > 0 { reg(t0, &format!("DEPURACIÓN → boton {cual:?} ({ms} ms){}{}", if n > 1 { ", soltado antes el anterior" } else { "" }, crate::volcado::marca())) }
                    continue;
                }
                let s = match orden {
                    Orden::Crudo { ssrc, carga } => Some((format!("crudo a {} ({} B)", super::protocolo::nombre_canal(ssrc), carga.len()), protocolo.depurar_crudo(ssrc, &carga, us()))),
                    Orden::Trama { ssrc, subtipo, carga } => Some((format!("trama {subtipo} a {} ({} B)", super::protocolo::nombre_canal(ssrc), carga.len()), protocolo.depurar_trama(ssrc, subtipo, &carga, us()))),
                    Orden::Mensaje { ruta, valor } => Some((format!("mensaje {ruta} {}", String::from_utf8_lossy(&valor).chars().take(200).collect::<String>()), protocolo.depurar_mensaje(&ruta, &valor, true, us()))),
                    Orden::Pulso => protocolo.pulso(us()).map(|s| ("pulso".to_owned(), s)),
                    // La parada va a la del programa (depurar::escuchar), nunca aquí.
                    Orden::Parar => None,
                    o => { reg(t0, &format!("DEPURACIÓN: {o:?} es de la ruta WebRTC")); None }
                };
                if let Some((que, s)) = s {
                    reg(t0, &format!("DEPURACIÓN → {que}: {}", s.carga.iter().take(64).map(|b| format!("{b:02x}")).collect::<String>()));
                    emisor.mandar(&socket, s).await;
                }
            },
            _ = micro_cada.tick(), if ajustes.micro.is_some() => {
                let m = ajustes.micro.as_ref().unwrap();
                reg(t0, &format!("micro: nivel {} · {} tramas de voz enviadas y {} de silencio (sin voz en cola) en 10 s · codificadas {}, tiradas {} en total",
                    m.nivel_dbfs().map(|d| format!("{d:.1} dBFS")).unwrap_or_else(|| "sin muestras".into()), micro_enviadas, micro_silencios,
                    m.codificadas.load(Ordering::Relaxed), m.tiradas.load(Ordering::Relaxed)));
                (micro_enviadas, micro_silencios) = (0, 0);
            },
            // En otra tarea: una respuesta lenta del servicio (plazo de 15 s) no puede
            // dejar muda la sesión, que la consola cierra al segundo sin respuesta.
            _ = keepalive.tick() => {
                let (x, fallido, et) = (xhome.clone(), keepalive_fallido.clone(), crate::relevo::ETIQUETA.try_with(|e| e.clone()).unwrap_or_default());
                tokio::spawn(async move {
                    // Sólo el primer fallo se registra.
                    if let Err(e) = x.keepalive().await { if !fallido.swap(true, Ordering::Relaxed) { crate::registro::registrar(t0, &format!("{et}keepalive HTTP: {e}")) } }
                });
            },
            _ = resumen.tick() => {
                reg(t0, &format!("audio nativo: {} tramas perdidas en la red (huecos de frameId, paso {:?}), {} fuera de orden; {} confirmaciones sueltas al núcleo en 30 s",
                    protocolo.audio_perdidas, protocolo.audio_paso, protocolo.audio_desordenadas, std::mem::take(&mut confirmaciones)));
                let total: u64 = protocolo.desconocidos.values().sum();
                if total != resumen_anterior {
                    resumen_anterior = total;
                    let mut clases: Vec<_> = protocolo.desconocidos.iter().map(|(k, n)| format!("{k}: {n}")).collect();
                    clases.sort();
                    reg(t0, &format!("desconocidos hasta ahora: {}", clases.join(" · ")));
                }
            }
            _ = vigilar.tick() => {
                if ultimo_dato.elapsed() > PLAZO_DATOS {
                    reg(t0, &format!("la consola dejó de mandar: {:.1} s sin un paquete que se descifre (plazo {} s)", ultimo_dato.elapsed().as_secs_f64(), PLAZO_DATOS.as_secs()));
                    for l in emisor.caja.volcar(ultimo_dato, Instant::now()) { reg(t0, &l) }
                    break Fin::Corte("la consola dejó de mandar".into())
                }
                if fallos_srtp > fallos_avisados { reg(t0, &format!("SRTP: {} paquetes que no se descifran", fallos_srtp - fallos_avisados)); fallos_avisados = fallos_srtp }
            }
            // en el búfer, sin copiar cada paquete a un Vec (lo de antes
            // de dar la conexión por buena, que es raro, se copia a él).
            r = async {
                match pendientes.pop() {
                    Some(p) => { let n = p.len().min(buf.len()); buf[..n].copy_from_slice(&p[..n]); Ok((n, emisor.destino)) }
                    None => socket.recv_from(&mut buf).await,
                }
            } => {
                let (n, origen) = match r {
                    Ok(r) => { tolerancia.bien(); r }
                    Err(e) => { emisor.caja.anotar(Suceso::ErrorRecibir); if let Some(t) = tolerancia.error(e)? { reg(t0, &t) } continue }
                };
                let paquete = &buf[..n];
                // Una hora por paquete, para el último dato y la caja negra.
                let ahora = Instant::now();
                if let Some(m) = stun::leer(paquete) {
                    emisor.caja.anotar_en(ahora, Suceso::StunRecibido(m.tipo));
                    // Consentimiento (RFC 7675): la consola sigue comprobando durante la sesión.
                    if m.tipo == stun::PETICION {
                        let ok = socket.send_to(&stun::respuesta(&m.transaccion, origen, &clave_remota), origen).await.is_ok();
                        emisor.caja.anotar(Suceso::StunMandado(ok));
                    }
                    continue;
                }
                if origen != emisor.destino { emisor.caja.anotar_en(ahora, Suceso::Ajeno); continue }
                let Some((pt, seq, ssrc)) = leer_cabecera(paquete) else { continue };
                let estimado = roc.estimar(seq);
                if emisor.srtp.descifrar_en(paquete, estimado, &mut claro).is_err() {
                    fallos_srtp += 1;
                    emisor.caja.anotar_en(ahora, Suceso::SinDescifrar(pt));
                    continue
                }
                roc.aceptar(seq, estimado);
                ultimo_dato = ahora;
                emisor.caja.anotar_en(ahora, Suceso::Recibido(pt, ssrc));
                let (salidas, eventos) = protocolo.recibir(pt, ssrc, &claro, us());
                for s in salidas { emisor.mandar(&socket, s).await }
                let mut corte = None;
                let mut pulso = false;
                for e in eventos {
                    match e {
                        Evento::Audio { frame_id, opus } => {
                            if !crate::relevo::tomar_audio(ajustes.numero, &reserva, t0) { continue }
                            medida.audio(t0);
                            medida.paquetes.fetch_add(1, Ordering::Relaxed);
                            medida.bytes.fetch_add(opus.len() as u64, Ordering::Relaxed);
                            if let Some(&toc) = opus.first() { if toc >> 3 >= 28 && toc & 4 != 0 { medida.celt_fb_estereo.fetch_add(1, Ordering::Relaxed); } }
                            if let Some(r) = &reserva {
                                let ahora = t0.elapsed().as_secs_f64() * 1000.0;
                                if let Ok(mut r) = r.lock() { r.recibir(frame_id as u16, opus, ahora); }
                            }
                        }
                        Evento::Nota(n) => reg(t0, &n),
                        Evento::Mensaje { clave, valor } => {
                            // Los mensajes con claves (p. ej. /streaming/server/setReconnectSrtpKey)
                            // no se escriben: sólo su huella.
                            let secreto = clave.to_lowercase().contains("key");
                            let texto = if secreto { format!("(oculto: {} B, huella {})", valor.len(), crate::xhome::huella(&valor)) } else if valor.len() <= 300 { valor.clone() } else { "(largo)".into() };
                            reg(t0, &format!("la consola → {clave} {texto}"));
                            if valor.contains("WarningForBeingIdle") {
                                pulso = true;
                            }
                            if clave.contains("sessionLifetimeManagement") || valor.contains("Kick") {
                                let motivo = serde_json::from_str::<Value>(&valor).ok().and_then(|v| v["reason"].as_str().map(str::to_owned)).unwrap_or_default();
                                if motivo == "KickByNewSession" { crate::relevo::kick() }
                                if motivo.starts_with("Kick") && motivo != "KickForStopCommand" { corte = Some(motivo) }
                            }
                        }
                        Evento::CanalCerrado(ssrc) => {
                            reg(t0, &format!("la consola cierra el canal ssrc {ssrc}"));
                            if ssrc == super::paquetes::SSRC_AUDIO { corte = Some("la consola cerró el canal de audio".into()) }
                        }
                        Evento::Desconexion => corte = Some("la consola cerró la sesión".into()),
                    }
                }
                if std::mem::take(&mut pulso) {
                    match protocolo.pulso(us()) {
                        Some(s) => { reg(t0, &format!("aviso de inactividad → pulso: {}", hex(&s.carga))); emisor.mandar(&socket, s).await }
                        None => reg(t0, "aviso de inactividad (sin canal Input: no hay pulso)"),
                    }
                }
                if let Some(m) = corte { break Fin::Corte(m) }
            }
        }
    };
    // Despedida por UDP antes del DELETE (no cuando no habrá DELETE para
    // re-engancharse: revisión 19 §1.5).
    let sin_delete = crate::xhome::reenganchar() && !matches!(fin, Fin::Cumplido);
    if !sin_delete { emisor.mandar(&socket, Protocolo::despedida()).await; }
    Ok(fin)
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn roc_da_la_vuelta() {
        let mut r = Roc::default();
        for s in [65_530u16, 65_535] { let e = r.estimar(s); r.aceptar(s, e) }
        assert_eq!(r.estimar(2), 1);
        r.aceptar(2, 1);
        assert_eq!((r.roc, r.estimar(65_534)), (1, 0));
    }

    #[test]
    fn candidatos_ida_y_vuelta() {
        let c = candidatos("192.168.1.5:50000".parse().unwrap(), "u", "p");
        let mut consola = c.clone();
        consola["Candidates"]["1"] = json!({ "transportAddress": "[fe80::1]:3074", "ipv6": "1" });
        assert_eq!(remotos(&consola), vec!["192.168.1.5:50000".parse::<SocketAddr>().unwrap()]);
        assert_eq!(c["Candidates"]["count"], 1);
    }
}
