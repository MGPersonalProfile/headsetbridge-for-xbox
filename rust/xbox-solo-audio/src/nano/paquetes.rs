//! Formatos de la ruta Nano V3 nativa (little endian), los de
//! `greenlight-native/packages/gamestreaming-protocol`. Sólo lo que usa un
//! cliente de solo audio: núcleo (sondeo de MTU, keepalive, URCP), cabecera
//! DCT, MuxDCTControl y MuxDCTChannel.

/// Tipos de carga RTP del núcleo y del control de canales.
pub const PT_SONDEO: u8 = 102;
pub const PT_KEEPALIVE: u8 = 101;
pub const PT_URCP: u8 = 100;
pub const PT_CONTROL: u8 = 97;
/// MuxDCTChannel: 35..=63.
pub const PT_CANAL: u8 = 35;

/// SSRC de cada canal.
pub const SSRC_NUCLEO: u32 = 0;
pub const SSRC_CONTROL: u32 = 1024;
pub const SSRC_QOS: u32 = 1025;
pub const SSRC_VIDEO: u32 = 1026;
pub const SSRC_AUDIO: u32 = 1027;
pub const SSRC_MENSAJES: u32 = 1028;
pub const SSRC_CHAT: u32 = 1029;
pub const SSRC_ENTRADA: u32 = 1030;
pub const SSRC_RESPUESTA: u32 = 1031;

/// MuxDCTChannel: tipo de paquete.
pub const MUX_NINGUNO: u16 = 0;
pub const MUX_TRAMA: u16 = 1;
pub const MUX_ABRIR: u16 = 2;
pub const MUX_DATOS: u16 = 3;
pub const MUX_CONFIG: u16 = 4;
pub const MUX_CONFIG_ACK: u16 = 5;

#[derive(Default)]
pub struct Escritor(pub Vec<u8>);

impl Escritor {
    pub fn u8(mut self, v: u8) -> Self { self.0.push(v); self }
    pub fn u16(mut self, v: u16) -> Self { self.0.extend_from_slice(&v.to_le_bytes()); self }
    pub fn u24(mut self, v: u32) -> Self { self.0.extend_from_slice(&v.to_le_bytes()[..3]); self }
    pub fn u32(mut self, v: u32) -> Self { self.0.extend_from_slice(&v.to_le_bytes()); self }
    pub fn u64(mut self, v: u64) -> Self { self.0.extend_from_slice(&v.to_le_bytes()); self }
    pub fn bytes(mut self, v: &[u8]) -> Self { self.0.extend_from_slice(v); self }
    pub fn fin(self) -> Vec<u8> { self.0 }
}

pub struct Lector<'a> { pub b: &'a [u8], pub pos: usize }

impl<'a> Lector<'a> {
    pub fn nuevo(b: &'a [u8]) -> Self { Self { b, pos: 0 } }
    pub fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let r = self.b.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(r)
    }
    pub fn u8(&mut self) -> Option<u8> { self.bytes(1).map(|b| b[0]) }
    pub fn u16(&mut self) -> Option<u16> { self.bytes(2).map(|b| u16::from_le_bytes([b[0], b[1]])) }
    pub fn u24(&mut self) -> Option<u32> { self.bytes(3).map(|b| u32::from_le_bytes([b[0], b[1], b[2], 0])) }
    pub fn u32(&mut self) -> Option<u32> { self.bytes(4).map(|b| u32::from_le_bytes(b.try_into().unwrap())) }
    pub fn u64(&mut self) -> Option<u64> { self.bytes(8).map(|b| u64::from_le_bytes(b.try_into().unwrap())) }
    pub fn resto(&mut self) -> &'a [u8] { let r = &self.b[self.pos.min(self.b.len())..]; self.pos = self.b.len(); r }
    pub fn quedan(&self) -> usize { self.b.len().saturating_sub(self.pos) }
}

// ---------------------------------------------------------------- núcleo

/// UDPConnectionProbing Syn: tipo 1, `largo - 2` bytes 0x01 y dos ceros.
pub fn sondeo_syn(largo: usize) -> Vec<u8> {
    let mut v = Escritor::default().u16(1).fin();
    v.resize(largo, 1);
    v.extend_from_slice(&[0, 0]);
    v
}

/// UDPConnectionProbing Ack: tipo 2, el MTU acordado y dos ceros.
pub fn sondeo_ack(largo: u16) -> Vec<u8> { Escritor::default().u16(2).u16(largo).u16(0).fin() }

pub enum Sondeo { Syn(usize), Ack(u16) }

pub fn leer_sondeo(c: &[u8]) -> Option<Sondeo> {
    let mut l = Lector::nuevo(c);
    match l.u16()? {
        // Lo que mide greenlight-native: el resto menos los dos ceros, más 2.
        1 => Some(Sondeo::Syn(c.len().checked_sub(2)?)),
        2 => Some(Sondeo::Ack(l.u16()?)),
        _ => None,
    }
}

pub const KA_NINGUNO: u32 = 0;
pub const KA_DESCONEXION: u32 = 4;
pub const KA_CONFIG: u32 = 9;
pub const KA_CONFIG_ACK: u32 = 18;

/// UDPKeepAlive: cabecera u32, tipo u32, dos ceros. La desconexión lleva
/// cabecera FFFFFFFF.
pub fn keepalive(cabecera: u32, tipo: u32) -> Vec<u8> {
    let cabecera = if tipo == KA_DESCONEXION { u32::MAX } else { cabecera };
    Escritor::default().u32(cabecera).u32(tipo).u16(0).fin()
}

/// (cabecera, tipo) de un UDPKeepAlive.
pub fn leer_keepalive(c: &[u8]) -> Option<(u32, u32)> {
    let mut l = Lector::nuevo(c);
    Some((l.u32()?, l.u32()?))
}

/// URCPControl Config (tipo 0) con los valores de greenlight-native.
pub fn urcp_config() -> Vec<u8> { Escritor::default().u16(0).u24(786_435).u32(100).u32(0).u32(5).u16(0).fin() }
/// URCPControl Accepted (tipo 1).
pub fn urcp_aceptado() -> Vec<u8> { Escritor::default().u16(1).u16(1).u32(0).u16(0).fin() }

pub fn leer_urcp(c: &[u8]) -> Option<u16> { Lector::nuevo(c).u16() }

// ---------------------------------------------------------------- cabecera DCT

#[derive(Debug, Default, PartialEq)]
pub struct CabeceraDct {
    pub secuencia: Option<u16>,
    pub confirma: Option<u16>,
    /// Bits de la cabecera que greenlight-native no interpreta (0x02, 0x20, 0x80 del primero).
    pub bits_desconocidos: u8,
}

/// Lee la cabecera DCT y devuelve (cabecera, cuerpo sin los dos bytes finales,
/// esos dos bytes). Orden y campos como `DCTPacket.readHeader()`.
pub fn leer_dct(c: &[u8]) -> Option<(CabeceraDct, &[u8], u16)> {
    let mut l = Lector::nuevo(c);
    let (f0, f1) = (l.u8()?, l.u8()?);
    let mut h = CabeceraDct { bits_desconocidos: f0 & 0xa2, ..Default::default() };
    if f0 & 0x01 != 0 {
        h.confirma = Some(l.u16()?);
        l.u24()?;
        let banderas = l.bytes(2)?;
        l.bytes(usize::from(banderas[1] & 0x0f))?;
    }
    if f1 & 0x01 != 0 { l.u24()?; }
    if f0 & 0x40 != 0 { l.u8()?; }
    if f0 & 0x10 != 0 { h.confirma = Some(l.u16()?); }
    if f0 & 0x04 != 0 { h.secuencia = Some(l.u16()?); }
    if f0 & 0x08 != 0 {
        h.confirma = Some(l.u16()?);
        let banderas = l.u8()?;
        l.u24()?;
        // 0x81 → 2 bytes, 0x82 → 3… (greenlight-native sólo conocía hasta 0x84). Con
        // la red degradada la consola manda 0x85 (6 bytes): antes se tiraba el
        // paquete entero, audio y acuses del núcleo, y la consola acababa cortando
        //.
        let retraso = match banderas { 0x81..=0x8f => usize::from(banderas - 0x7f), _ => return None };
        l.bytes(retraso)?;
    }
    let cuerpo = l.resto();
    let corte = cuerpo.len().checked_sub(2)?;
    Some((h, &cuerpo[..corte], u16::from_le_bytes([cuerpo[corte], cuerpo[corte + 1]])))
}

/// `DCTPacket.packHeader`: con confirmación, 05c0 + confirma + marca de 24 bits
/// + 0000 + secuencia; sin ella, 04c0 + secuencia.
pub fn empaquetar_dct(carga: &[u8], confirma: Option<u16>, marca: u32, secuencia: u16) -> Vec<u8> {
    let e = match confirma {
        Some(c) => Escritor::default().bytes(&[0x05, 0xc0]).u16(c).u24(marca & 0xff_ffff).u16(0),
        None => Escritor::default().bytes(&[0x04, 0xc0]),
    };
    e.u16(secuencia).bytes(carga).fin()
}

/// La confirmación sin datos del núcleo (cada 50 ms si llegó algo nuevo):
/// 01c0 + confirma + marca + 0000 + carga 0000.
pub fn confirmacion_nucleo(confirma: u16, marca: u32) -> Vec<u8> {
    Escritor::default().bytes(&[0x01, 0xc0]).u16(confirma).u24(marca & 0xff_ffff).u16(0).u16(0).fin()
}

// ---------------------------------------------------------------- MuxDCTControl

pub enum Control<'a> { Abrir { nombre: &'a str }, Confirmar, Cerrar }

/// Cuerpo de un MuxDCTControl (sin cabecera DCT ni bytes finales).
pub fn leer_control(cuerpo: &[u8]) -> Option<Control<'_>> {
    let mut l = Lector::nuevo(cuerpo);
    match l.u32()? {
        2 => { let n = l.u16()?; Some(Control::Abrir { nombre: std::str::from_utf8(l.bytes(n.into())?).ok()? }) }
        3 => Some(Control::Confirmar),
        // ChannelClose (como el tipo 4 del Nano antiguo): la consola lo manda
        // al cerrar el canal de audio si se le pide un códec que no ofrece
        //.
        4 => Some(Control::Cerrar),
        _ => None,
    }
}

/// MuxDCTControl Confirm con datos, más los dos bytes finales a cero.
pub fn control_confirmar(datos: &[u8]) -> Vec<u8> {
    Escritor::default().u32(3).u32(datos.len() as u32).bytes(datos).u16(0).fin()
}

// ---------------------------------------------------------------- MuxDCTChannel

/// tipo, secuencia, u32 a cero, datos y la «siguiente secuencia».
pub fn canal(tipo: u16, secuencia: u16, siguiente: u16, datos: &[u8]) -> Vec<u8> {
    Escritor::default().u16(tipo).u16(secuencia).u32(0).bytes(datos).u16(siguiente).fin()
}

/// (tipo, datos) del cuerpo de un MuxDCTChannel. El tipo 0 sin datos es un
/// paquete vacío; con datos, una trama de audio (el «hack» de greenlight-native).
pub fn leer_canal(cuerpo: &[u8]) -> Option<(u16, &[u8])> {
    if cuerpo.is_empty() { return Some((MUX_NINGUNO, &[])) }
    let mut l = Lector::nuevo(cuerpo);
    let tipo = l.u16()?;
    if tipo != MUX_NINGUNO { l.u16()?; }
    l.u32()?;
    Some((tipo, l.resto()))
}

/// OpenChannel Request con el UDID (canal Control).
pub fn abrir_peticion_udid(udid: &str) -> Vec<u8> {
    Escritor::default().u16(1).u16(0).u32(0).u32(0).u32(0).u16(0).u32(udid.len() as u32).bytes(udid.as_bytes()).fin()
}
/// OpenChannel Request con unknown2 = 4 (canal Messaging).
pub fn abrir_peticion_mensajes() -> Vec<u8> { Escritor::default().u16(1).u16(0).u32(4).u32(1).fin() }
/// OpenChannel Response sin UDID (canal QoS); greenlight-native deja unknown1 en 1.
pub fn abrir_respuesta() -> Vec<u8> { Escritor::default().u16(2).u16(0).u32(1).fin() }
/// OpenChannel AudioAck del canal Audio: frameId, formato 2 (Audio), 48 kHz y
/// el campo final (el códec: 0 = Opus; la consola no ofrece otro), a 0.
pub fn abrir_audio_ack(frame_id: u32, ultimo: u32) -> Vec<u8> { Escritor::default().u32(frame_id).u32(2).u32(48_000).u32(ultimo).fin() }
/// OpenChannel Audio del canal ChatAudio (el cliente propone 24 kHz).
pub fn abrir_chat(marca_us: u64) -> Vec<u8> {
    Escritor::default().u16(7).u16(0).u64(marca_us).u32(1).u32(1).u32(24_000).u32(1).u32(2).u32(0).fin()
}

pub enum Abrir { Peticion, Respuesta, Audio, AudioAck, Otro(u16) }

pub fn leer_abrir(datos: &[u8]) -> Option<Abrir> {
    Some(match Lector::nuevo(datos).u16()? { 1 => Abrir::Peticion, 2 => Abrir::Respuesta, 7 => Abrir::Audio, t if t > 255 => Abrir::AudioAck, t => Abrir::Otro(t) })
}

pub const DATOS_VARIOS: u16 = 1;
pub const DATOS_MENSAJE: u16 = 2;
pub const DATOS_DATOS: u16 = 3;
pub const DATOS_TRAMA: u16 = 4;
/// Data subtipo 5: lo manda la consola por Control ~200 ms tras conectar,
/// con 8 bytes a cero. No pide respuesta.
pub const DATOS_CONTROL_SERVIDOR: u16 = 5;
pub const DATOS_CONTROL: u16 = 7;
pub const DATOS_ACK: u16 = 16;

/// Data Control (canal Control) con los valores de greenlight-native: siete
/// campos (u32, u16, u32, u32, u32, u32, u24) sin nombre; ¿5 000 000 = bitrate?
pub fn datos_control() -> Vec<u8> {
    Escritor::default().u16(DATOS_CONTROL).u32(5_000_000).u16(1).u32(3_489_660_933).u32(2).u32(50_331_648).u32(0).u24(0).fin()
}
/// Data Frame vacío (canal Control: «todos los canales listos»).
pub fn datos_trama_vacia() -> Vec<u8> { Escritor::default().u16(DATOS_TRAMA).u16(1).fin() }
/// Data Ack (canal Audio).
pub fn datos_ack() -> Vec<u8> { Escritor::default().u16(DATOS_ACK).u16(0).fin() }
/// Data MultiMessage vacío: «política recibida» (canal QoS).
pub fn datos_varios_vacio() -> Vec<u8> { Escritor::default().u16(DATOS_VARIOS).u16(0).fin() }

pub const SIN_ACK: u32 = 0;
pub const PIDE_ACK: u32 = 1;
pub const ES_ACK: u32 = 2;

/// Data Message clave-valor (canal Messaging).
pub fn datos_mensaje(ack: u32, frame_id: u32, clave: &str, valor: &[u8]) -> Vec<u8> {
    Escritor::default().u16(DATOS_MENSAJE).u16(0).u32(28 + (clave.len() + valor.len()) as u32).u32(ack).u32(frame_id).u32(1)
        .u32(clave.len() as u32).u32(valor.len() as u32).u32(0).u32((clave.len() + valor.len()) as u32)
        .bytes(clave.as_bytes()).bytes(valor).fin()
}

pub struct Mensaje<'a> { pub ack: u32, pub frame_id: u32, pub clave: &'a str, pub valor: &'a [u8] }

/// Datos de un Data Message (sin el subtipo).
pub fn leer_mensaje(datos: &[u8]) -> Option<Mensaje<'_>> {
    let mut l = Lector::nuevo(datos);
    l.u16()?; l.u32()?;
    let (ack, frame_id, tipo) = (l.u32()?, l.u32()?, l.u32()?);
    if tipo != 1 { return None }
    let (nc, nv) = (l.u32()?, l.u32()?);
    l.u32()?; l.u32()?;
    Some(Mensaje { ack, frame_id, clave: std::str::from_utf8(l.bytes(nc as usize)?).ok()?, valor: l.bytes(nv as usize)? })
}

pub struct Parte<'a> { pub total: u32, pub desplazamiento: u32, pub datos: &'a [u8] }

/// Una parte de un Data MultiMessage (sin el subtipo); None si es el vacío.
pub fn leer_parte(datos: &[u8]) -> Option<Parte<'_>> {
    let mut l = Lector::nuevo(datos);
    l.u16()?;
    if l.quedan() == 0 { return None }
    let (total, _paquetes, desplazamiento, n) = (l.u32()?, l.u32()?, l.u32()?, l.u32()?);
    Some(Parte { total, desplazamiento, datos: l.bytes(n as usize)? })
}

/// Data Data Video «VideoAck2» (canal Video): confirma una trama.
pub fn datos_video_ack(frame_id: u32, marca_ms: u32) -> Vec<u8> {
    Escritor::default().u16(DATOS_DATOS).u16(0).u32(1).u32(16).u32(128).u32(frame_id).u32(marca_ms).u32(0).fin()
}

pub struct TramaAudio<'a> { pub frame_id: u32, pub marca: u32, pub opus: &'a [u8] }

/// Datos de un MuxDCTChannel tipo 0 con audio (DataDataAudioFormat).
pub fn leer_audio(datos: &[u8]) -> Option<TramaAudio<'_>> {
    let mut l = Lector::nuevo(datos);
    l.u16()?;
    if l.u32()? != 4 { return None }
    let (frame_id, marca) = (l.u32()?, l.u32()?);
    l.u32()?;
    let n = l.u32()?;
    Some(TramaAudio { frame_id, marca, opus: l.bytes(n as usize)? })
}

pub const CONFIG_VIDEO_SERVIDOR: u16 = 1;
pub const CONFIG_ENTRADA: u16 = 5;

/// Config VideoClient: lo que pide el cliente (greenlight-native: 1280×720, 60 fps, códec 0).
pub fn config_video_cliente(frame_id: u32, fps: u32, ancho: u32, alto: u32) -> Vec<u8> {
    Escritor::default().u16(2).u16(0).u32(1).u32(20).u32(frame_id).u32(fps).u32(ancho).u32(alto).u32(0).fin()
}
/// ConfigAck Video.
pub fn config_ack_video() -> Vec<u8> { Escritor::default().u16(3).u16(0).u32(1).u32(4).u32(48).fin() }
/// Config Input (canal InputFeedback): pantalla de 1280×720 y sin táctil.
pub fn config_entrada(frame_id: u32) -> Vec<u8> {
    Escritor::default().u16(CONFIG_ENTRADA).u16(0).u32(24).u32(4).u32(11).u32(1280).u32(720).u32(0).u32(frame_id).fin()
}
/// Config InputAck (canal Input).
pub fn config_entrada_ack(marca_us: u64) -> Vec<u8> {
    Escritor::default().u16(6).u16(0).u32(20).u32(4).u32(11).u32(0).u64(marca_us).fin()
}
/// frameId de un Config Input (sin el subtipo).
pub fn leer_config_entrada(datos: &[u8]) -> Option<u32> {
    let mut l = Lector::nuevo(datos);
    l.bytes(2 + 4 * 6)?;
    l.u32()
}

pub const TRAMA_ACK: u32 = 3;
pub const TRAMA_VIDEO: u32 = 4;
pub const TRAMA_ENTRADA: u32 = 7;

pub struct TramaVideo { pub frame_id: u32, pub total: u32, pub paquetes: u32, pub unknown2: u32, pub datos: usize }

/// FrameVideoFormat.
pub fn leer_video(datos: &[u8]) -> Option<TramaVideo> {
    let mut l = Lector::nuevo(datos);
    l.u32()?; l.u32()?;
    let unknown2 = l.u32()?;
    let frame_id = l.u32()?;
    l.u64()?; l.u32()?;
    let (total, paquetes) = (l.u32()?, l.u32()?);
    l.u32()?;
    let (metadatos, n) = (l.u32()?, l.u32()?);
    l.bytes(metadatos as usize)?;
    l.bytes(n as usize)?;
    Some(TramaVideo { frame_id, total, paquetes, unknown2, datos: n as usize })
}

/// Frame Ack (canal InputFeedback).
pub fn trama_ack(frame_id: u32) -> Vec<u8> { Escritor::default().u32(TRAMA_ACK).u32(4).u32(frame_id).fin() }

/// Bloque de estadísticas de la trama de entrada (constante en greenlight-native).
fn estadisticas() -> Vec<u8> { Escritor::default().u16(0).u16(1).u32(2).u32(8).u32(799).u32(836).fin() }

/// El bloque de mando (43 bytes, `Frame/Input/Gamepad.ts` de greenlight-native):
/// byte 0 sin nombre; 1-16, un contador por botón que sube en cada cambio
/// (cruceta arriba, abajo, izquierda, derecha, start, select, sticks, bumpers,
/// nexus, ?, A, B, X, Y); 17-18, gatillos; 19-26, ejes i16 (izquierdo X e Y,
/// derecho X e Y); 27-34 sin nombre; 35-36, los botones pulsados (máscara, u16);
/// 37-42, máscara de ejes y dos sin nombre.
pub type Mando = [u8; 43];
pub const MANDO_EJE_X: usize = 19;
pub const MANDO_BOTONES: usize = 35;

/// Trama de entrada con el mando tal como está (pulso, botón de depuración).
pub fn trama_mando(frame_id: u32, marca_us: u64, mando: &Mando) -> Vec<u8> {
    trama_entrada(frame_id, marca_us, Some(mando), true)
}

/// Frame Input: con `mando`, el bloque de mando; con `estadisticas`, el bloque
/// de estadísticas (greenlight-native lo manda siempre). Sin ninguno, una trama
/// vacía.
pub fn trama_entrada(frame_id: u32, marca_us: u64, mando: Option<&Mando>, estadisticas_: bool) -> Vec<u8> {
    let mando = mando.map(|m| m.to_vec()).unwrap_or_default();
    let est = if estadisticas_ { estadisticas() } else { Vec::new() };
    Escritor::default().u32(TRAMA_ENTRADA).u32(19 + (mando.len() + est.len()) as u32).u32(frame_id).u64(marca_us).u16(0)
        .u16(u16::from(!mando.is_empty())).bytes(&mando).u8(u8::from(!est.is_empty())).bytes(&est).u16(0).fin()
}

/// Una trama de audio del cliente (formato de las de la consola,
/// DataDataAudioFormat) dentro de un MuxDCTChannel tipo 0.
pub fn trama_audio(frame_id: u32, marca: u32, opus: &[u8]) -> Vec<u8> {
    Escritor::default().u16(0).u32(0).u16(0).u32(4).u32(frame_id).u32(marca).u32(0).u32(opus.len() as u32).bytes(opus).u16(0).fin()
}

#[cfg(test)]
mod pruebas {
    #[test]
    fn cabecera_con_retraso_de_seis_bytes() {
        // Un acuse del núcleo real de la consola con la red degradada (02-10, juego).
        let p: Vec<u8> = (0..32).step_by(2).map(|i| u8::from_str_radix(&"08c06e4f85435c060040ffffff030000"[i..i + 2], 16).unwrap()).collect();
        let (h, cuerpo, sec) = leer_dct(&p).expect("antes daba None");
        assert_eq!((h.confirma, cuerpo.len(), sec), (Some(0x4f6e), 0, 0));
    }

    use super::*;
    use crate::nano::srtp::{leer_cabecera, Srtp, CABECERA};

    /// Clave de los vectores de greenlight-native (`tests/packets/config.ts`).
    const CLAVE: &str = "vV9cuxwCpZ2iKGVFJhdLBcQ2mfSRzFvPj7+vTQbq";

    fn claro(paquete: &[u8]) -> Vec<u8> { Srtp::nuevo(CLAVE).unwrap().descifrar(paquete, 0).unwrap() }
    /// Cuerpo y bytes finales de un vector, tras la cabecera DCT.
    fn cuerpo(paquete: &[u8]) -> (CabeceraDct, Vec<u8>, u16) {
        let c = claro(paquete);
        let (h, cuerpo, fin) = leer_dct(&c).unwrap();
        (h, cuerpo.to_vec(), fin)
    }
    macro_rules! vector { ($n:literal) => { include_bytes!(concat!("../../pruebas/nano/", $n)) } }

    #[test]
    fn nucleo() {
        assert_eq!(claro(vector!("URCPControl_Config.bin")), urcp_config());
        assert_eq!(claro(vector!("UDPKeepAlive_Config_Response.bin")), keepalive(40, KA_CONFIG_ACK));
        assert_eq!(claro(vector!("UDPKeepAlive_Disconnect.bin")), keepalive(0, KA_DESCONEXION));
        assert_eq!(leer_keepalive(&claro(vector!("UDPKeepAlive_Config_Request.bin"))), Some((0, KA_CONFIG)));
        let syn = claro(vector!("UDPConnectionProbing_Syn.bin"));
        assert!(matches!(leer_sondeo(&syn), Some(Sondeo::Syn(n)) if n == syn.len() - 2));
        assert!(matches!(leer_sondeo(&claro(vector!("UDPConnectionProbing_Ack.bin"))), Some(Sondeo::Ack(1386))));
        assert_eq!(sondeo_syn(6), vec![1, 0, 1, 1, 1, 1, 0, 0]);
        assert_eq!(leer_urcp(&claro(vector!("URCPControl_Config_Accepted.bin"))), Some(1));
    }

    #[test]
    fn cabecera_dct() {
        // 04c0: sólo secuencia.
        let (h, c, fin) = cuerpo(vector!("MuxDCTControl_Confirm_Audio.bin"));
        assert_eq!(h.secuencia, Some(0x6a));
        assert_eq!((c, fin), (control_confirmar(&[])[..8].to_vec(), 0));
        // 51c1: confirmación con marca, c1, u8, confirmación; paquete vacío.
        let (h, c, _) = cuerpo(vector!("MuxDCTChannel_None.bin"));
        assert_eq!((h.confirma, h.secuencia, c.len()), (Some(0x64), None, 0));
        // 45c0 con 3 bytes de cabecera extra (AudioAck del cliente).
        let (h, c, _) = cuerpo(vector!("MuxDCTChannel_Control_Audio2.bin"));
        assert_eq!((h.confirma, h.secuencia), (Some(0x76), Some(0x72)));
        assert_eq!(leer_canal(&c).unwrap().0, MUX_ABRIR);
        // 0cc0: secuencia y cabecera de conexión caída.
        let (h, _, _) = cuerpo(vector!("MuxDCTChannel_Data_Video_Ack2.bin"));
        assert!(h.secuencia.is_some() && h.confirma.is_some());
    }

    #[test]
    fn empaquetar_como_greenlight() {
        assert_eq!(empaquetar_dct(&[0xaa], None, 0, 0x6a), vec![0x04, 0xc0, 0x6a, 0, 0xaa]);
        assert_eq!(empaquetar_dct(&[], Some(0x6f), 0x4bab, 0x6c), vec![0x05, 0xc0, 0x6f, 0, 0xab, 0x4b, 0, 0, 0, 0x6c, 0]);
        assert_eq!(confirmacion_nucleo(0x64, 0x0743), vec![0x01, 0xc0, 0x64, 0, 0x43, 0x07, 0, 0, 0, 0, 0]);
    }

    /// Lo que manda el cliente, byte a byte contra lo que mandaba el de referencia.
    #[test]
    fn paquetes_del_cliente() {
        let datos = |n: &[u8]| { let (_, c, fin) = cuerpo(n); (c, fin) };
        let entero = |(c, fin): (Vec<u8>, u16)| { let mut v = c; v.extend_from_slice(&fin.to_le_bytes()); v };
        assert_eq!(entero(datos(vector!("MuxDCTControl_Confirm_Control.bin"))), control_confirmar(&[3, 0]));
        assert_eq!(entero(datos(vector!("MuxDCTChannel_Control_Openchannel1.bin"))), canal(MUX_ABRIR, 0, 0, &abrir_peticion_udid("4BDB3609-C1F1-4195-9B37-FEFF45DA8B8E")));
        assert_eq!(entero(datos(vector!("MuxDCTChannel_Data_Control.bin"))), canal(MUX_DATOS, 0, 1, &datos_control()));
        assert_eq!(entero(datos(vector!("MuxDCTChannel_Data_Control2.bin"))), canal(MUX_DATOS, 1, 2, &datos_trama_vacia()));
        assert_eq!(entero(datos(vector!("MuxDCTChannel_Data_Qos.bin"))), canal(MUX_DATOS, 0, 1, &datos_varios_vacio()));
        assert_eq!(entero(datos(vector!("MuxDCTChannel_Data_Ack.bin"))), canal(MUX_DATOS, 0, 1, &datos_ack()));
        assert_eq!(entero(datos(vector!("MuxDCTChannel_Control_Messaging.bin"))), canal(MUX_ABRIR, 0, 0, &abrir_peticion_mensajes()));
        assert_eq!(entero(datos(vector!("MuxDCTChannel_Control_Audio2.bin"))), canal(MUX_ABRIR, 0, 0, &abrir_audio_ack(0xdc51_2aa3, 0)));
        assert_eq!(entero(datos(vector!("MuxDCTChannel_ConfigAck_Video.bin"))), canal(MUX_CONFIG_ACK, 0, 1, &config_ack_video()));
        let (c, _) = datos(vector!("MuxDCTChannel_Data_Messaging_Messaging1.bin"));
        let (_, d) = leer_canal(&c).unwrap();
        let m = leer_mensaje(&d[2..]).unwrap();
        assert_eq!(m.clave, "/streaming/systemUi/configuration");
        assert_eq!(&d[..], &datos_mensaje(m.ack, m.frame_id, m.clave, m.valor)[..]);
    }

    #[test]
    fn audio_del_servidor() {
        let (_, c, _) = cuerpo(vector!("MuxDCTChannel_Data_Audio_Data.bin"));
        let (tipo, d) = leer_canal(&c).unwrap();
        assert_eq!(tipo, MUX_NINGUNO);
        let a = leer_audio(d).unwrap();
        assert_eq!((a.frame_id, a.opus.len(), a.opus[0]), (0xdc51_2aa3, 0x21a, 0xfc));
        // La del cliente se lee igual.
        let propia = trama_audio(9, 3, &[0xf8, 0xff, 0xfe]);
        let (tipo, d) = leer_canal(&propia[..propia.len() - 2]).unwrap();
        let a = leer_audio(d).unwrap();
        assert_eq!((tipo, a.frame_id, a.marca, a.opus), (MUX_NINGUNO, 9, 3, &[0xf8, 0xff, 0xfe][..]));
    }

    #[test]
    fn qos_por_partes() {
        let mut total = 0;
        for n in [&vector!("MuxDCTChannel_Data_Qos_Qos1.bin")[..], vector!("MuxDCTChannel_Data_Qos_Qos2.bin")] {
            let (_, c, _) = cuerpo(n);
            let (tipo, d) = leer_canal(&c).unwrap();
            assert_eq!((tipo, Lector::nuevo(d).u16()), (MUX_DATOS, Some(DATOS_VARIOS)));
            let p = leer_parte(&d[2..]).unwrap();
            assert_eq!(p.total, 0x3054);
            total += p.datos.len();
        }
        assert_eq!(total, 2 * 0x529);
    }

    #[test]
    fn video_y_entrada() {
        let (_, c, _) = cuerpo(vector!("MuxDCTChannel_Frame_Video_Start.bin"));
        let (tipo, d) = leer_canal(&c).unwrap();
        assert_eq!((tipo, Lector::nuevo(d).u32()), (MUX_TRAMA, Some(TRAMA_VIDEO)));
        let v = leer_video(&d[4..]).unwrap();
        assert_eq!((v.frame_id, v.total, v.paquetes), (823_738_637, 75, 1));
        let (_, c, _) = cuerpo(vector!("MuxDCTChannel_Data_Video_Ack.bin"));
        let (_, d) = leer_canal(&c).unwrap();
        assert_eq!(&d[..], &datos_video_ack(0x3119_410f, 0x23ec_b1df)[..]);
        let (_, c, _) = cuerpo(vector!("MuxDCTChannel_Frame_Ack.bin"));
        assert_eq!(&leer_canal(&c).unwrap().1[..], &trama_ack(0xd707_cbff)[..]);
        // Trama de entrada con mando (Input_Gamepad): mismo tamaño que la nuestra con mando.
        let (_, c, _) = cuerpo(vector!("MuxDCTChannel_Frame_Input_Gamepad.bin"));
        let (_, d) = leer_canal(&c).unwrap();
        assert_eq!(d.len(), trama_entrada(1, 2, Some(&[0; 43]), true).len());
        assert_eq!(trama_entrada(1, 2, None, true).len(), 4 + 4 + 19 + 20);
        assert_eq!(trama_entrada(1, 2, None, false).len(), 4 + 4 + 19);
    }

    fn hex(s: &str) -> Vec<u8> { (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect() }

    /// Los paquetes nuevos de la consola de 2026, en claro (sesión local, 01-10).
    #[test]
    fn paquetes_de_la_consola_actual() {
        for (claro, seq) in [
            ("04c080000300000000000000050000000000000000000100", 0x80),
            ("04c106000083000300000000000000050000000000000000000100", 0x83),
            ("05c07f00bd9f00050094000300000000000000050000000000000000000100", 0x94),
        ] {
            let b = hex(claro);
            let (h, cuerpo, fin) = leer_dct(&b).unwrap();
            let (tipo, datos) = leer_canal(cuerpo).unwrap();
            assert_eq!((h.secuencia, tipo, fin), (Some(seq), MUX_DATOS, 1));
            assert_eq!(datos, &[5, 0, 0, 0, 0, 0, 0, 0, 0, 0][..]);
        }
        for claro in ["05c072003637000001007e0004000000000000000000", "04c0820004000000000000000000", "14c10901008300830004000000000000000000"] {
            let b = hex(claro);
            let (_, cuerpo, _) = leer_dct(&b).unwrap();
            assert!(matches!(leer_control(cuerpo), Some(Control::Cerrar)));
        }
    }

    #[test]
    fn todos_los_vectores_se_leen() {
        let srtp = Srtp::nuevo(CLAVE).unwrap();
        for e in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/pruebas/nano")).unwrap() {
            let ruta = e.unwrap().path();
            let b = std::fs::read(&ruta).unwrap();
            let Ok(c) = srtp.descifrar(&b, 0) else { continue };
            let (pt, _, _) = leer_cabecera(&b).unwrap();
            assert!(b.len() > CABECERA);
            if pt == PT_CONTROL || (35..=63).contains(&pt) {
                let (_, cuerpo, _) = leer_dct(&c).unwrap_or_else(|| panic!("{}", ruta.display()));
                if pt != PT_CONTROL { assert!(leer_canal(cuerpo).is_some(), "{}", ruta.display()) }
            }
        }
    }
}
