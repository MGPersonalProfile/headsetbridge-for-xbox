//! Oferta de comprobación local. `create_offer` no señaliza ni abre una sesión xHome.

use std::sync::Arc;

use crate::canales::CANALES;
use std::sync::OnceLock;

use webrtc::api::interceptor_registry::{configure_nack, configure_rtcp_reports, register_default_interceptors};
use webrtc::api::media_engine::MediaEngine;
use webrtc::data_channel::RTCDataChannel;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::rtp_transceiver::rtp_codec::{RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType};
use webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection;
use webrtc::rtp_transceiver::RTCRtpTransceiverInit;

/// Qué interceptores RTP/RTCP lleva la conexión (`--interceptores=`). Por defecto
/// los de webrtc-rs: NACK, informes RTCP y TWCC de recepción, como un navegador.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Interceptores {
    Defecto,
    /// NACK e informes RTCP, sin TWCC.
    Informes,
    Ninguno,
}

impl Interceptores {
    pub fn desde(texto: &str) -> Option<Self> {
        match texto { "defecto" => Some(Self::Defecto), "informes" => Some(Self::Informes), "ninguno" => Some(Self::Ninguno), _ => None }
    }
}

/// `--nack` (diagnóstico): pide a la consola que reenvíe los paquetes de audio
/// perdidos (`a=rtcp-fb:111 nack`) y los pide a los 5 ms, no a los 100 ms del
/// generador por defecto: con 4 ms de ida y vuelta en la LAN, el reenvío llega
/// dentro de la reserva de 20 ms. Por defecto webrtc-rs sólo hace NACK del vídeo.
pub static NACK_AUDIO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Audio redundante (RFC 2198, `red/48000/2` PT 63 con `111/111`, como Chromium),
/// ofrecido antes que Opus. La consola lo acepta y manda en cada paquete una copia
/// del anterior: recupera ~2/3 de las pérdidas de la Wi-Fi (medido el 01-10).
/// Lo activa `main` salvo con `--sin-red`. Pedir 2 copias no sirve: manda una.
pub static RED_AUDIO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Copias redundantes pedidas en el fmtp de RED (`--red=N`; 1 = `111/111`).
pub static RED_COPIAS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);

/// Lo fija `main` una vez al arrancar; si no, `Defecto`.
pub static INTERCEPTORES: OnceLock<Interceptores> = OnceLock::new();

#[derive(Debug, Eq, PartialEq)]
pub struct CanalCreado {
    pub nombre: String,
    pub protocolo: String,
    pub ordenado: bool,
}

pub struct OfertaLocal {
    pub sdp: String,
    pub canales: Vec<CanalCreado>,
}

/// Oferta local para comprobarla sin consola (lo que imprime `main` sin argumentos).
pub async fn generar_oferta() -> webrtc::error::Result<OfertaLocal> {
    let (pc, canales_creados) = crear_conexion().await?;
    let canales = canales_creados.iter().map(|canal| CanalCreado {
        nombre: canal.label().into(),
        protocolo: canal.protocol().into(),
        ordenado: canal.ordered(),
    }).collect();
    let sdp = crear_oferta(&pc).await?;
    pc.close().await?;
    Ok(OfertaLocal { sdp, canales })
}

/// La oferta tal como se manda a la consola: puerto 9 en el vídeo inactive.
pub async fn crear_oferta(pc: &RTCPeerConnection) -> webrtc::error::Result<String> {
    // Una sección inactive con puerto 0 es «rechazada» y la consola no responde
    //; werift lo hace y el prototipo en Node lo corrige igual.
    Ok(pc.create_offer(None).await?.sdp.replacen("\r\nm=video 0 ", "\r\nm=video 9 ", 1))
}

/// La conexión con la oferta mínima que acepta la consola y los cuatro canales,
/// en el orden de xbox-xcloud-player: canales, audio sendrecv, vídeo inactive.
pub async fn crear_conexion() -> webrtc::error::Result<(Arc<RTCPeerConnection>, Vec<Arc<RTCDataChannel>>)> {
    let mut medios = MediaEngine::default();
    if RED_AUDIO.load(std::sync::atomic::Ordering::Relaxed) {
        medios.register_codec(RTCRtpCodecParameters {
            capability: RTCRtpCodecCapability {
                mime_type: "audio/red".into(),
                clock_rate: 48_000,
                channels: 2,
                sdp_fmtp_line: vec!["111"; 1 + usize::from(RED_COPIAS.load(std::sync::atomic::Ordering::Relaxed).max(1))].join("/"),
                ..Default::default()
            },
            payload_type: 63,
            ..Default::default()
        }, RTPCodecType::Audio)?;
    }
    // Los predeterminados añaden muchos códecs; la consola aceptó sólo estos PT.
    medios.register_codec(RTCRtpCodecParameters {
        capability: RTCRtpCodecCapability {
            mime_type: "audio/opus".into(),
            clock_rate: 48_000,
            channels: 2,
            sdp_fmtp_line: "minptime=10;useinbandfec=1;stereo=1".into(),
            ..Default::default()
        },
        payload_type: 111,
        ..Default::default()
    }, RTPCodecType::Audio)?;
    medios.register_codec(RTCRtpCodecParameters {
        capability: RTCRtpCodecCapability {
            mime_type: "video/H264".into(),
            clock_rate: 90_000,
            sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=0;profile-level-id=42e01f".into(),
            ..Default::default()
        },
        payload_type: 115,
        ..Default::default()
    }, RTPCodecType::Video)?;

    let nack_audio = NACK_AUDIO.load(std::sync::atomic::Ordering::Relaxed);
    if nack_audio {
        medios.register_feedback(webrtc::rtp_transceiver::RTCPFeedback { typ: "nack".to_owned(), parameter: "".to_owned() }, RTPCodecType::Audio);
    }
    // Informes de recepción y NACK como un navegador; `--interceptores=` los quita para medir.
    let registro = match INTERCEPTORES.get().copied().unwrap_or(Interceptores::Defecto) {
        Interceptores::Defecto => register_default_interceptors(Registry::new(), &mut medios)?,
        Interceptores::Informes => configure_rtcp_reports(configure_nack(Registry::new(), &mut medios)),
        Interceptores::Ninguno => Registry::new(),
    };
    let registro = if nack_audio {
        let mut r = registro;
        r.add(Box::new(webrtc::interceptor::nack::generator::Generator::builder().with_interval(std::time::Duration::from_millis(5))));
        r
    } else { registro };
    let api = APIBuilder::new().with_media_engine(medios).with_interceptor_registry(registro).build();
    let pc = Arc::new(api.new_peer_connection(RTCConfiguration::default()).await?);
    let mut canales = Vec::with_capacity(CANALES.len());
    for especificacion in CANALES {
        canales.push(pc.create_data_channel(especificacion.nombre, Some(RTCDataChannelInit {
            ordered: Some(true),
            protocol: Some(especificacion.protocolo.into()),
            ..Default::default()
        })).await?);
    }

    // Con RED en recepción: webrtc-rs no tiene empaquetador de RED y falla al
    // preparar el envío si el transceptor envía («codec does not have a payloader»).
    let red = RED_AUDIO.load(std::sync::atomic::Ordering::Relaxed);
    pc.add_transceiver_from_kind(RTPCodecType::Audio, Some(RTCRtpTransceiverInit {
        direction: if red { RTCRtpTransceiverDirection::Recvonly } else { RTCRtpTransceiverDirection::Sendrecv },
        send_encodings: vec![],
    })).await?;
    // webrtc-rs sólo crea transceptores sendrecv o recvonly
    // (ErrPeerConnAddTransceiverFromKindSupport con inactive): se crea en
    // recepción y se pasa a inactive antes de la oferta, como hace el cliente
    // ligero con Chromium (soloaudio-player.ts). Sin vídeo, la consola no lo codifica.
    let video = pc.add_transceiver_from_kind(RTPCodecType::Video, Some(RTCRtpTransceiverInit {
        direction: RTCRtpTransceiverDirection::Recvonly,
        send_encodings: vec![],
    })).await?;
    video.set_direction(RTCRtpTransceiverDirection::Inactive).await;
    Ok((pc, canales))
}

pub fn redactar_sdp(sdp: &str) -> String {
    sdp.lines()
        .map(str::trim_end)
        .filter(|linea| ![
            "o=", "c=", "a=ice-ufrag:", "a=ice-pwd:", "a=fingerprint:",
            "a=candidate:", "a=end-of-candidates", "a=ssrc:", "a=msid:",
        ].iter().any(|prefijo| linea.starts_with(prefijo)))
        .collect::<Vec<_>>()
        .join("\n")
}
