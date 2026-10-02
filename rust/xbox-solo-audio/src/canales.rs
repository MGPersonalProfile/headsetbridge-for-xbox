//! El formato que se ha probado en el prototipo en Node, sin acceso a Xbox.
//! Los JSON viajan como bytes UTF-8 por SCTP, nunca como `send_text`.

#[cfg(feature = "webrtc")]
use bytes::Bytes;
#[cfg(feature = "webrtc")]
use webrtc::data_channel::RTCDataChannel;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Canal {
    pub nombre: &'static str,
    pub protocolo: &'static str,
}

pub const CANALES: [Canal; 4] = [
    Canal { nombre: "input", protocolo: "1.0" },
    Canal { nombre: "chat", protocolo: "chatV1" },
    Canal { nombre: "control", protocolo: "controlV1" },
    Canal { nombre: "message", protocolo: "messageV1" },
];

const ACCESS_KEY: &str = "4BDB3609-C1F1-4195-9B37-FEFF45DA8B8E";

#[derive(Clone, Copy, Debug, Default)]
pub struct TramaMando {
    pub lx: f64,
    pub ly: f64,
}

fn cabecera(tipo: u16, secuencia: u32, tiempo_ms: f64) -> [u8; 14] {
    let mut bytes = [0; 14];
    bytes[0..2].copy_from_slice(&tipo.to_le_bytes());
    bytes[2..6].copy_from_slice(&secuencia.to_le_bytes());
    bytes[6..14].copy_from_slice(&tiempo_ms.to_le_bytes());
    bytes
}

/// Un paquete de `input` sin ningún informe: sólo la cabecera (tipo 0).
pub fn paquete_vacio(secuencia: u32, tiempo_ms: f64) -> Vec<u8> {
    cabecera(0, secuencia, tiempo_ms).to_vec()
}

/// Puntero (tipo 4) con un fotograma y ningún evento: el formato de
/// `_writePointerData` en el reproductor de Greenlight (`packet.ts:249`).
pub fn paquete_puntero_vacio(secuencia: u32, tiempo_ms: f64) -> Vec<u8> {
    let mut bytes = cabecera(4, secuencia, tiempo_ms).to_vec();
    bytes.extend_from_slice(&[1, 0]);
    bytes
}

/// Declara el cliente táctil (`touchinputenabledchanged` a true).
pub fn tactil_activo(id: &str) -> Vec<u8> {
    mensaje("/streaming/characteristics/touchinputenabledchanged", "{\"touchInputEnabled\":true}", id)
}

/// El tipo 8 es ClientMetadata (xcloud-rs `packets/input.rs`, bit 0x08).
pub fn paquete_metadatos(secuencia: u32, tiempo_ms: f64, max_touchpoints: u8) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(15);
    bytes.extend_from_slice(&cabecera(8, secuencia, tiempo_ms));
    bytes.push(max_touchpoints);
    bytes
}

fn eje(valor: f64) -> i16 {
    // JS trunca hacia cero después de limitar a ±32767; usar `round` cambiaría el mando.
    (valor * 32767.0).trunc().clamp(-32767.0, 32767.0) as i16
}

pub fn paquete_mando(secuencia: u32, tiempo_ms: f64, tramas: &[TramaMando]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(14 + 1 + 23 * tramas.len());
    bytes.extend_from_slice(&cabecera(2, secuencia, tiempo_ms));
    bytes.push(tramas.len() as u8);
    for trama in tramas {
        bytes.push(indice_mando()); // índice: el mando remoto que anuncia control.
        bytes.extend_from_slice(&0u16.to_le_bytes()); // botones.
        bytes.extend_from_slice(&eje(trama.lx).to_le_bytes());
        bytes.extend_from_slice(&eje(-trama.ly).to_le_bytes());
        for _ in 0..2 { bytes.extend_from_slice(&0i16.to_le_bytes()); }
        for _ in 0..2 { bytes.extend_from_slice(&0u16.to_le_bytes()); }
        let fisicidad = FISICIDAD.load(std::sync::atomic::Ordering::Relaxed);
        bytes.extend_from_slice(&fisicidad.to_le_bytes());
        bytes.extend_from_slice(&fisicidad.to_be_bytes());
    }
    bytes
}

fn entrecomillar(texto: &str) -> String {
    serde_json::to_string(texto).expect("serializar un &str no puede fallar")
}

pub fn handshake(id: &str) -> Vec<u8> {
    format!("{{\"type\":\"Handshake\",\"version\":\"messageV1\",\"id\":{},\"cv\":\"\"}}", entrecomillar(id)).into_bytes()
}

pub fn solicitud_autorizacion() -> Vec<u8> {
    format!("{{\"message\":\"authorizationRequest\",\"accessKey\":\"{ACCESS_KEY}\"}}").into_bytes()
}

/// Índice del mando remoto (`--indice-mando=N`, diagnóstico; por defecto 0).
pub static INDICE_MANDO: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// `--fisicidad=N` (diagnóstico): valor de los dos campos «physicality» de la
/// trama (físico en LE y virtual en BE, como stratix `InputPacket.swift:147`). 0 = como Greenlight.
pub static FISICIDAD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
/// `--systemui=2` (diagnóstico): `systemUi/configuration` versión [0,2,0] como stratix.
pub static SYSTEMUI_V2: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn indice_mando() -> u8 { INDICE_MANDO.load(std::sync::atomic::Ordering::Relaxed) }

pub fn cambio_mando() -> Vec<u8> {
    format!(r#"{{"message":"gamepadChanged","gamepadIndex":{},"wasAdded":true}}"#, indice_mando()).into_bytes()
}

/// El mando remoto se va (`Control.ts:33-40`, `sendGamepadRemoved`).
pub fn quitar_mando() -> Vec<u8> {
    format!(r#"{{"message":"gamepadChanged","gamepadIndex":{},"wasAdded":false}}"#, indice_mando()).into_bytes()
}

/// Respuesta a un `TransactionStart` de la consola (stratix `MessageChannel.swift:197-230`).
pub fn transaccion_completa(id: &str, contenido: &str) -> Vec<u8> {
    format!("{{\"type\":\"TransactionComplete\",\"content\":{},\"id\":{},\"cv\":\"\"}}", entrecomillar(contenido), entrecomillar(id)).into_bytes()
}

fn mensaje(target: &str, contenido: &str, id: &str) -> Vec<u8> {
    format!(
        "{{\"type\":\"Message\",\"content\":{},\"id\":{},\"target\":{},\"cv\":\"\"}}",
        entrecomillar(contenido), entrecomillar(id), entrecomillar(target)
    ).into_bytes()
}

/// `indice` sigue exactamente las seis emisiones posteriores a HandshakeAck.
pub fn configuracion(indice: usize, id: &str, client_app_install_id: &str) -> Option<Vec<u8>> {
    let (target, contenido) = match indice {
        0 => ("/streaming/systemUi/configuration", if SYSTEMUI_V2.load(std::sync::atomic::Ordering::Relaxed) { "{\"version\":[0,2,0],\"systemUis\":[]}" } else { "{\"version\":[0,1,0],\"systemUis\":[]}" }.to_owned()),
        1 => ("/streaming/properties/clientappinstallidchanged", format!("{{\"clientAppInstallId\":{}}}", entrecomillar(client_app_install_id))),
        2 => ("/streaming/characteristics/orientationchanged", "{\"orientation\":0}".to_owned()),
        3 => ("/streaming/characteristics/touchinputenabledchanged", "{\"touchInputEnabled\":false}".to_owned()),
        4 => ("/streaming/characteristics/clientdevicecapabilities", "{}".to_owned()),
        5 => ("/streaming/characteristics/dimensionschanged", "{\"horizontal\":1920,\"vertical\":1080,\"preferredWidth\":1920,\"preferredHeight\":1080,\"safeAreaLeft\":0,\"safeAreaTop\":0,\"safeAreaRight\":1920,\"safeAreaBottom\":1080,\"supportsCustomResolution\":true}".to_owned()),
        _ => return None,
    };
    Some(mensaje(target, &contenido, id))
}

/// El tipo `Bytes` llama a `RTCDataChannel::send`, que selecciona PPID binario.
#[cfg(feature = "webrtc")]
pub async fn enviar_binario(canal: &RTCDataChannel, datos: &[u8]) -> webrtc::error::Result<usize> {
    canal.send(&Bytes::copy_from_slice(datos)).await
}
