//! Una sesión de solo audio contra la consola: lo mismo que el prototipo en Node
//! y `canales.mjs`, sin reintentos todavía. Por ahora mide el audio recibido
//! (paquetes, kbps, modo Opus); la reproducción llega en el paso siguiente.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;

use crate::audio::ReservaCompartida;
use crate::canales::{transaccion_completa, paquete_puntero_vacio, tactil_activo, paquete_vacio, quitar_mando, cambio_mando, configuracion, enviar_binario, handshake, paquete_mando, paquete_metadatos, solicitud_autorizacion, TramaMando};
use crate::oferta::{crear_conexion, crear_oferta};
use crate::xhome::XHome;

/// `--sin-mando` (diagnóstico): ni anuncio del mando ni tramas. La consola
/// echa la sesión a los ~58 s (KickForNoClientConnection), pero sirve para ver
/// si el mando remoto es lo que pausa la música de la consola (01-10).
pub static SIN_MANDO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `--latido=cliente|vacio|mensaje` (diagnóstico, implica sin mando): qué se
/// manda cada segundo en vez de las tramas del mando, para ver si basta para
/// que la consola no eche la sesión (KickForNoClientConnection a los ~58 s).
/// 0 = no, 1 = ClientMetadata, 2 = input vacío, 3 = configuración por message,
/// 4 = cliente táctil (touchInputEnabled true y puntero sin eventos).
pub static LATIDO: AtomicU64 = AtomicU64::new(0);

/// `--anuncio=fugaz` (diagnóstico): el mando se anuncia y se quita al instante,
/// como stratix (`ControlChannel.swift:68-69`), y las tramas siguen.
pub static ANUNCIO_FUGAZ: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `--mando-inicial=S` (diagnóstico): el mando remoto sólo los S primeros
/// segundos de cada sesión; después se quita (gamepadChanged wasAdded:false) y
/// no se mandan más tramas. 0 = siempre. ¿Basta el mando al principio para que
/// la consola no eche la sesión, y deja así de pausar la música?
pub static MANDO_INICIAL_S: AtomicU64 = AtomicU64::new(0);

type Resultado<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub use crate::medida::{Fin, Medida};

fn registro(t0: Instant, texto: &str) {
    crate::relevo::reg(t0, texto);
}

fn uuid() -> String { uuid::Uuid::new_v4().to_string() }

/// Conecta y mantiene la sesión hasta `hasta`, o hasta un corte (Fin::Corte); para con DELETE.
/// `numero`: el de la sesión en un relevo (`--relevo`), 0 fuera de él.
#[allow(clippy::too_many_arguments)]
pub async fn sesion(mut xhome: XHome, consola: &str, hasta: tokio::time::Instant, prueba_corte: Option<Duration>, t0: Instant, medida: Arc<Medida>, reserva: Option<ReservaCompartida>, parar: Arc<tokio::sync::Notify>, numero: u64) -> Resultado<Fin> {
    let et = crate::relevo::etiqueta(numero);
    let (pc, canales) = crear_conexion().await?;
    let canal = |nombre: &str| canales.iter().find(|c| c.label() == nombre).cloned().expect("los cuatro canales existen");
    let (input, control, message) = (canal("input"), canal("control"), canal("message"));

    // Audio: cuenta paquetes y el modo que eligió el codificador (byte TOC, RFC 6716 §3.1).
    let m = medida.clone();
    // Sólo la PRIMERA fuente de audio (SSRC) entra en la reserva. Con música, la
    // consola abrió una segunda y las dos, mezcladas en una reserva con
    // numeraciones distintas, daban saltos, ocultaciones y recortes (01-10:
    // 83 paq/s, 55 ocultados y 102 recortados en un minuto).
    let elegida = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let et_pista = et.clone();
    pc.on_track(Box::new(move |pista, _, _| {
        let (m, reserva, elegida, et) = (m.clone(), reserva.clone(), elegida.clone(), et_pista.clone());
        Box::pin(async move {
            if pista.kind() != webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Audio { return }
            // Las llamadas de webrtc-rs van en sus propias tareas: la etiqueta, a mano.
            let registro = |t0: Instant, texto: &str| crate::registro::registrar(t0, &format!("{et}{texto}"));
            registro(t0, &format!("pista de audio: ssrc {:08x}, {} ({})", pista.ssrc(), pista.codec().capability.mime_type, pista.id()));
            let mut vistas: Vec<u32> = Vec::new();
            while let Ok((rtp, _)) = pista.read_rtp().await {
                let ssrc = rtp.header.ssrc;
                let _ = elegida.compare_exchange(0, ssrc, Ordering::Relaxed, Ordering::Relaxed);
                if ssrc != elegida.load(Ordering::Relaxed) {
                    if !vistas.contains(&ssrc) {
                        vistas.push(ssrc);
                        registro(t0, &format!("otra fuente de audio: ssrc {ssrc:08x}, PT {}, {} B; no entra en la reserva", rtp.header.payload_type, rtp.payload.len()));
                    }
                    m.otros.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                // Relevo: sólo la sesión más nueva que da audio alimenta la reserva.
                if !crate::relevo::tomar_audio(numero, &reserva, t0) { continue }
                if let Some(r) = &reserva {
                    let ahora = t0.elapsed().as_secs_f64() * 1000.0;
                    if let Ok(mut r) = r.lock() {
                        let seq = rtp.header.sequence_number;
                        // RED (PT 63): primero las copias de paquetes anteriores (si
                        // aquél se perdió y aún no sonó, lo rellenan), luego el principal.
                        match (rtp.header.payload_type == 63).then(|| desempaquetar_red(&rtp.payload)).flatten() {
                            Some(bloques) => {
                                for (retroceso, opus) in bloques.iter().filter(|(k, o)| *k > 0 && !o.is_empty()) {
                                    m.redundantes.fetch_add(1, Ordering::Relaxed);
                                    r.recibir(seq.wrapping_sub(*retroceso), opus.to_vec(), ahora);
                                }
                                if let Some((_, principal)) = bloques.last() { r.recibir(seq, principal.to_vec(), ahora); }
                            }
                            None => { r.recibir(seq, rtp.payload.to_vec(), ahora); }
                        }
                    }
                }
                m.audio(t0);
                m.paquetes.fetch_add(1, Ordering::Relaxed);
                m.bytes.fetch_add(rtp.payload.len() as u64, Ordering::Relaxed);
                // CELT FB = config 28-31 (toc >> 3); estéreo = bit 2.
                if let Some(&toc) = rtp.payload.first() {
                    if toc >> 3 >= 28 && toc & 4 != 0 { m.celt_fb_estereo.fetch_add(1, Ordering::Relaxed); }
                }
            }
        })
    }));
    let et_estado = et.clone();
    pc.on_peer_connection_state_change(Box::new(move |estado| {
        crate::registro::registrar(t0, &format!("{et_estado}conexión: {estado}"));
        Box::pin(async {})
    }));

    // Protocolo: Handshake al abrir `message`; tras HandshakeAck, autorización,
    // anuncio del mando, metadatos, configuración y una trama en reposo por
    // segundo (sin el anuncio Y las tramas, KickForNoClientConnection a los 58 s).
    let secuencia = Arc::new(Mutex::new(0u32));
    let fin = Arc::new(tokio::sync::Notify::new());
    let motivo_corte: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    {
        let m2 = message.clone();
        message.on_open(Box::new(move || {
            let m2 = m2.clone();
            Box::pin(async move { let _ = enviar_binario(&m2, &handshake(&uuid())).await; })
        }));
    }
    {
        let (input, control, message2, secuencia, fin, motivo_corte, et) = (input.clone(), control.clone(), message.clone(), secuencia.clone(), fin.clone(), motivo_corte.clone(), et.clone());
        message.on_message(Box::new(move |msg: DataChannelMessage| {
            let (input, control, message2, secuencia, fin, motivo_corte, et) = (input.clone(), control.clone(), message2.clone(), secuencia.clone(), fin.clone(), motivo_corte.clone(), et.clone());
            Box::pin(async move {
                let registro = |t0: Instant, texto: &str| crate::registro::registrar(t0, &format!("{et}{texto}"));
                let texto = String::from_utf8_lossy(&msg.data).to_string();
                let json: serde_json::Value = serde_json::from_str(&texto).unwrap_or_default();
                // Qué manda la consola y cuándo (tipo y ruta; el contenido sólo en los cortos).
                let ruta = json["target"].as_str().unwrap_or("");
                let contenido = json["content"].as_str().unwrap_or("");
                registro(t0, &format!("la consola → {} {ruta} {}", json["type"].as_str().unwrap_or("?"), if contenido.len() <= 160 { contenido } else { "(largo)" }));
                // Como los clientes de referencia: toda transacción se cierra
                // (los diálogos, con Result 0 = aceptar).
                if json["type"] == "TransactionStart" {
                    if let Some(id) = json["id"].as_str() {
                        let contenido = if ruta.contains("ShowMessageDialog") { "{\"Result\":0}" } else { "" };
                        let _ = enviar_binario(&message2, &transaccion_completa(id, contenido)).await;
                    }
                }
                if json["type"] == "HandshakeAck" {
                    arrancar_tras_ack(&input, &control, &message2, secuencia.clone(), t0).await;
                }
                if texto.contains("WarningForBeingIdle") {
                    // Un toque de stick por debajo de la zona muerta (medido el 01-10).
                    let mut s = secuencia.lock().await;
                    *s += 1;
                    let _ = enviar_binario(&input, &paquete_mando(*s, ms(t0), &[TramaMando { lx: 0.1, ly: 0.0 }, TramaMando::default()])).await;
                    registro(t0, "aviso de inactividad → pulso");
                }
                if texto.contains("sessionLifetimeManagement") {
                    let motivo = serde_json::from_str::<serde_json::Value>(json["content"].as_str().unwrap_or("{}")).unwrap_or_default()["reason"].as_str().unwrap_or("").to_owned();
                    registro(t0, &format!("la consola: {motivo}"));
                    // KickForStopCommand responde a nuestro DELETE; el resto de Kick* es una expulsión.
                    if motivo.starts_with("Kick") && motivo != "KickForStopCommand" {
                        *motivo_corte.lock().await = Some(motivo);
                        fin.notify_one();
                    }
                }
            })
        }));
    }

    // Todo lo local ANTES del /play: la consola echa a la sesión
    // anterior en el /play mismo (KickByNewSession, medido: 1-12 ms), así que
    // en el relevo el hueco empieza ahí. Oferta, descripción local y candidatos
    // ICE quedan listos; tras Provisioned sólo falta intercambiar y conectar.
    let oferta = crear_oferta(&pc).await?;
    pc.set_local_description(RTCSessionDescription::offer(oferta.clone())?).await?;
    let mut reunidos = pc.gathering_complete_promise().await;
    let _ = reunidos.recv().await;
    let locales: Vec<String> = pc.local_description().await.map(|d| d.sdp).unwrap_or_default()
        .lines().filter_map(|l| l.trim_end().strip_prefix("a=").filter(|c| c.starts_with("candidate:")).map(str::to_owned)).collect();

    medida.marcar_play(t0);
    let play = Instant::now();
    let desde = move || play.elapsed().as_millis();
    xhome.arrancar(consola).await?;
    registro(t0, "sesión creada");
    xhome.esperar_consola(|e| registro(t0, &format!("consola: {e} (+{} ms del /play)", desde()))).await?;

    let respuesta = xhome.intercambiar_sdp(&oferta).await?;
    // Qué códecs de audio eligió la consola (sólo rtpmap y fmtp del m=audio).
    {
        let mut en_audio = false;
        let mut elegidos = Vec::new();
        for l in respuesta.lines() {
            if l.starts_with("m=") { en_audio = l.starts_with("m=audio"); if en_audio { elegidos.push(l.trim().to_owned()) } }
            else if en_audio && (l.starts_with("a=rtpmap") || l.starts_with("a=fmtp") || l.starts_with("a=rtcp-fb")) { elegidos.push(l.trim().to_owned()) }
        }
        registro(t0, &format!("respuesta de audio: {}", elegidos.join(" | ")));
    }
    pc.set_remote_description(RTCSessionDescription::answer(respuesta)?).await?;
    registro(t0, &format!("oferta aceptada (+{} ms del /play)", desde()));

    let remotos = xhome.intercambiar_ice(&locales).await?;
    registro(t0, &format!("candidatos: {} locales, {} de la consola (+{} ms del /play)", locales.len(), remotos.len(), desde()));
    for c in remotos {
        if let Err(e) = pc.add_ice_candidate(RTCIceCandidateInit { candidate: c, sdp_mid: Some("0".into()), sdp_mline_index: Some(0), username_fragment: None }).await {
            registro(t0, &format!("candidato rechazado: {e}"));
        }
    }

    // Keepalive cada 30 s, tramas de mando a 1 Hz y vigilancia cada segundo:
    // la consola manda 50 paquetes/s también en silencio, así que 3 s sin
    // ninguno es un corte (como el prototipo en Node).
    let mut keepalive = crate::red::intervalo(Duration::from_secs(30));
    let mut latido = crate::red::intervalo(Duration::from_secs(1));
    let limite = tokio::time::sleep_until(hasta);
    tokio::pin!(limite);
    let inicio = Instant::now();
    let (mut ultimos, mut ultimo_avance) = (medida.paquetes.load(Ordering::Relaxed), Instant::now());
    let mut corte_provocado = false;
    let mut par_registrado = false;
    let mut mando_quitado = false;
    let mut tactil_declarado = false;
    let resultado = loop {
        tokio::select! {
            _ = &mut limite => { registro(t0, "tiempo cumplido"); break Fin::Cumplido }
            _ = fin.notified() => break Fin::Corte(motivo_corte.lock().await.clone().unwrap_or_default()),
            _ = parar.notified() => break Fin::Cumplido,
            orden = crate::depurar::siguiente() => {
                use crate::depurar::Orden;
                match orden {
                    Orden::Control(b) => { let _ = enviar_binario(&control, &b).await; }
                    Orden::Message(b) => { let _ = enviar_binario(&message, &b).await; }
                    Orden::Input(b) => { let _ = enviar_binario(&input, &b).await; }
                    Orden::Mando(true) => { mando_quitado = false; let _ = enviar_binario(&control, &cambio_mando()).await; }
                    Orden::Mando(false) => { let _ = enviar_binario(&control, &quitar_mando()).await; }
                    Orden::Tramas(si) => { SIN_MANDO.store(!si, Ordering::Relaxed); if si { mando_quitado = false } }
                    Orden::Pulso => {
                        let mut s = secuencia.lock().await;
                        *s += 1;
                        let _ = enviar_binario(&input, &paquete_mando(*s, ms(t0), &[TramaMando { lx: 0.1, ly: 0.0 }, TramaMando::default()])).await;
                    }
                    // La parada va a la del programa (depurar::escuchar), nunca aquí.
                    Orden::Parar => {}
                    Orden::Boton { .. } => registro(t0, "DEPURACIÓN: boton es de la ruta nativa"),
                    Orden::Crudo { .. } | Orden::Trama { .. } | Orden::Mensaje { .. } => registro(t0, "DEPURACIÓN: crudo, trama y mensaje son de la ruta nativa (--nativo)"),
                }
            }
            // En su tarea, como en la ruta nativa (revisión 19 §1.1): una respuesta
            // lenta del servicio (plazo de 15 s) no puede parar el bucle de la sesión.
            _ = keepalive.tick() => {
                let x = xhome.clone();
                tokio::spawn(crate::relevo::ETIQUETA.scope(crate::relevo::ETIQUETA.try_with(|e| e.clone()).unwrap_or_default(), async move {
                    if let Err(e) = x.keepalive().await { registro(t0, &format!("keepalive: {e}")) }
                }));
            }
            _ = latido.tick() => {
                match LATIDO.load(Ordering::Relaxed) {
                    1 | 2 if input.ready_state() == webrtc::data_channel::data_channel_state::RTCDataChannelState::Open => {
                        let mut s = secuencia.lock().await;
                        *s += 1;
                        let paquete = if LATIDO.load(Ordering::Relaxed) == 1 { paquete_metadatos(*s, ms(t0), 2) } else { paquete_vacio(*s, ms(t0)) };
                        let _ = enviar_binario(&input, &paquete).await;
                    }
                    4 if input.ready_state() == webrtc::data_channel::data_channel_state::RTCDataChannelState::Open => {
                        if !tactil_declarado {
                            tactil_declarado = true;
                            let _ = enviar_binario(&message, &tactil_activo(&uuid())).await;
                        }
                        let mut s = secuencia.lock().await;
                        *s += 1;
                        let _ = enviar_binario(&input, &paquete_puntero_vacio(*s, ms(t0))).await;
                    }
                    3 if message.ready_state() == webrtc::data_channel::data_channel_state::RTCDataChannelState::Open => {
                        if let Some(m) = configuracion(0, &uuid(), "") { let _ = enviar_binario(&message, &m).await; }
                    }
                    _ => {}
                }
                let inicial = MANDO_INICIAL_S.load(Ordering::Relaxed);
                if inicial > 0 && !mando_quitado && inicio.elapsed() >= Duration::from_secs(inicial) {
                    mando_quitado = true;
                    let _ = enviar_binario(&control, &quitar_mando()).await;
                    registro(t0, "DIAGNÓSTICO: mando remoto quitado; sin más tramas");
                }
                if input.ready_state() == webrtc::data_channel::data_channel_state::RTCDataChannelState::Open && !SIN_MANDO.load(Ordering::Relaxed) && !mando_quitado {
                    let mut s = secuencia.lock().await;
                    *s += 1;
                    let _ = enviar_binario(&input, &paquete_mando(*s, ms(t0), &[TramaMando::default()])).await;
                }
                // El camino elegido (en casa, host ↔ host por la LAN): para medir el RTT aparte.
                if !par_registrado && pc.connection_state() == RTCPeerConnectionState::Connected {
                    if let Some(par) = pc.sctp().transport().ice_transport().get_selected_candidate_pair().await {
                        registro(t0, &format!("par ICE: {par}"));
                        par_registrado = true;
                    }
                }
                let ahora = medida.paquetes.load(Ordering::Relaxed);
                if ahora != ultimos { ultimos = ahora; ultimo_avance = Instant::now(); }
                if ultimo_avance.elapsed() > PLAZO_AUDIO { break Fin::Corte("audio parado".into()) }
                if matches!(pc.connection_state(), RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed) {
                    break Fin::Corte("conexión perdida".into())
                }
                // Diagnóstico: cerrar la sesión en la consola como si se cayera.
                if let Some(plazo) = prueba_corte {
                    if !corte_provocado && inicio.elapsed() >= plazo {
                        corte_provocado = true;
                        registro(t0, "PRUEBA: corto la sesión en la consola");
                        let _ = xhome.parar().await;
                    }
                }
            }
        }
    };
    if crate::xhome::reenganchar() && !matches!(resultado, Fin::Cumplido) {
        registro(t0, "DIAGNÓSTICO: sesión sin DELETE, para re-engancharse");
    } else if let Err(e) = xhome.parar().await { registro(t0, &format!("no se pudo cerrar la sesión en la consola: {e}")) }
    pc.close().await?;
    Ok(resultado)
}

const PLAZO_AUDIO: Duration = Duration::from_secs(3);

/// Desempaqueta RED (RFC 2198): devuelve (retroceso en paquetes de 20 ms, carga
/// Opus) de cada bloque, el principal el último con retroceso 0. `None` si está mal formado.
pub fn desempaquetar_red(carga: &[u8]) -> Option<Vec<(u16, &[u8])>> {
    let mut cabeceras = Vec::new();
    let mut i = 0;
    loop {
        let b = *carga.get(i)?;
        if b & 0x80 == 0 { i += 1; break }
        let h = carga.get(i..i + 4)?;
        let retroceso_ts = (u16::from(h[1]) << 6) | (u16::from(h[2]) >> 2);
        let largo = (usize::from(h[2] & 0x03) << 8) | usize::from(h[3]);
        cabeceras.push((retroceso_ts, largo));
        i += 4;
    }
    let mut bloques = Vec::new();
    for (retroceso_ts, largo) in cabeceras {
        bloques.push((retroceso_ts / 960, carga.get(i..i + largo)?));
        i += largo;
    }
    bloques.push((0, carga.get(i..)?));
    Some(bloques)
}

fn ms(t0: Instant) -> f64 { t0.elapsed().as_secs_f64() * 1000.0 }

async fn arrancar_tras_ack(input: &Arc<RTCDataChannel>, control: &Arc<RTCDataChannel>, message: &Arc<RTCDataChannel>, secuencia: Arc<Mutex<u32>>, t0: Instant) {
    let _ = enviar_binario(control, &solicitud_autorizacion()).await;
    if !SIN_MANDO.load(Ordering::Relaxed) {
        let _ = enviar_binario(control, &cambio_mando()).await;
        if ANUNCIO_FUGAZ.load(Ordering::Relaxed) { let _ = enviar_binario(control, &crate::canales::quitar_mando()).await; }
        let s = *secuencia.lock().await;
        let _ = enviar_binario(input, &paquete_metadatos(s, ms(t0), 2)).await;
    }
    let instalacion = uuid();
    for i in 0.. {
        let Some(m) = configuracion(i, &uuid(), &instalacion) else { break };
        let _ = enviar_binario(message, &m).await;
    }
    registro(t0, "HandshakeAck → control, input y configuración enviados");
}
