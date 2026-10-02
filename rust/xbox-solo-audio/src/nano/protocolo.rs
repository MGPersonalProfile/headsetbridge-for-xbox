//! La máquina de estados del cliente Nano V3 nativo, sin red ni relojes: recibe
//! cargas en claro y devuelve lo que hay que mandar. Sigue
//! `greenlight-native/packages/gamestreaming/src/` (index.ts y channels/*.ts)
//! con los mismos valores; lo que la consola actual manda y aquél no conoce se
//! anota una vez y se ignora.

use std::collections::{HashMap, HashSet};

use super::paquetes::*;

/// Lo que hay que mandar: tipo de carga RTP, SSRC y carga en claro.
#[derive(Debug, Clone, PartialEq)]
pub struct Salida { pub pt: u8, pub ssrc: u32, pub carga: Vec<u8> }

#[derive(Debug, PartialEq)]
pub enum Evento {
    Audio { frame_id: u32, opus: Vec<u8> },
    /// Algo que conviene registrar (canal abierto, mensaje de la consola, paquete desconocido).
    Nota(String),
    /// Un mensaje clave-valor de la consola por el canal Messaging.
    Mensaje { clave: String, valor: String },
    /// La consola cerró un canal (MuxDCTControl tipo 4).
    CanalCerrado(u32),
    /// La consola cerró la sesión (UDPKeepAlive Disconnect).
    Desconexion,
}

/// Qué tramas de entrada se mandan una vez la consola abre el canal Input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Entrada {
    /// Sólo el bloque de estadísticas (lo que hace greenlight-native).
    Estadisticas,
    /// Estadísticas y un mando en reposo.
    Mando,
    /// Tramas sin mando ni estadísticas (experimento del reto).
    Vacia,
    /// Ninguna: la consola echa la sesión a los ~57 s.
    Ninguna,
}

#[derive(Debug, Clone)]
pub struct Opciones {
    pub entrada: Entrada,
    /// Dejar de mandar tramas de entrada pasados estos µs desde el arranque.
    pub entrada_hasta_us: Option<u64>,
    /// Resolución y fps pedidos para el vídeo (no se decodifica: sólo se confirma).
    pub video: (u32, u32, u32),
    /// Cuánto más esperar el acuse de nuestra sonda de MTU tras la última, en ms (`--espera-mtu=`).
    pub espera_mtu_ms: u32,
    /// `--party-activa`: `setPartyChatActive` a true al abrir Messaging.
    pub party_activa: bool,
    /// Tipo RTP de las tramas de ChatAudio (`--chat-pt=`; 39, el del audio de la consola).
    pub chat_pt: u8,
    /// el primer Syn de la consola se acusa en el acto,
    /// como greenlight-native (`core.ts`), sin agotar nuestras 16 sondas: la
    /// consola manda antes el Config (~200 ms). El audio va en paquetes de
    /// cientos de bytes: el MTU (1232 en vez de 1434) no importa.
    pub mtu_en_el_acto: bool,
}

impl Default for Opciones {
    fn default() -> Self { Self { entrada: Entrada::Estadisticas, entrada_hasta_us: None, video: (60, 1280, 720), espera_mtu_ms: 0, party_activa: false, chat_pt: 39, mtu_en_el_acto: false } }
}

/// los botones de la orden de depuración `boton` (para medir la
/// latencia con el «tic» de navegación): (contador en el bloque de mando, máscara),
/// de `helpers/gamepad.ts` de greenlight-native.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Boton { Arriba, Abajo, Izquierda, Derecha, A, B }

impl Boton {
    pub fn de(texto: &str) -> Option<Self> {
        Some(match texto { "arriba" => Self::Arriba, "abajo" => Self::Abajo, "izquierda" => Self::Izquierda, "derecha" => Self::Derecha, "a" => Self::A, "b" => Self::B, _ => return None })
    }
    fn bits(self) -> (usize, u16) {
        match self { Self::Arriba => (1, 0x0001), Self::Abajo => (2, 0x0002), Self::Izquierda => (3, 0x0004), Self::Derecha => (4, 0x0008), Self::A => (13, 0x1000), Self::B => (14, 0x2000) }
    }
}

/// El nombre de un canal por su SSRC y al revés (para la depuración).
pub fn nombre_canal(ssrc: u32) -> &'static str {
    match ssrc { SSRC_NUCLEO => "núcleo", SSRC_CONTROL => "control", SSRC_QOS => "qos", SSRC_VIDEO => "video", SSRC_AUDIO => "audio", SSRC_MENSAJES => "messaging", SSRC_CHAT => "chataudio", SSRC_ENTRADA => "input", SSRC_RESPUESTA => "inputfeedback", _ => "?" }
}

pub fn ssrc_de(nombre: &str) -> Option<u32> {
    Some(match nombre { "control" => SSRC_CONTROL, "qos" => SSRC_QOS, "video" => SSRC_VIDEO, "audio" => SSRC_AUDIO, "messaging" | "mensajes" => SSRC_MENSAJES, "chataudio" | "chat" => SSRC_CHAT, "input" | "entrada" => SSRC_ENTRADA, "inputfeedback" | "respuesta" => SSRC_RESPUESTA, n => return n.parse().ok() })
}

/// El UDID que manda greenlight-native en el canal Control.
const UDID: &str = "4BDB3609-C1F1-4195-9B37-FEFF45DA8B8E";
/// Sondeos de MTU: de 1436 bytes bajando de 16 en 16, cada 10 ms, 16 como mucho.
const SONDEOS: u32 = 16;

/// Tamaño máximo aceptado para la política de QoS de la consola.
const QOS_MAXIMO: u32 = 64 * 1024;

pub struct Protocolo {
    opciones: Opciones,
    cliente_seq: u16,
    servidor_seq: u16,
    servidor_cambio: bool,
    pub mtu: Option<usize>,
    sondeos: u32,
    mensajes_seq: u16,
    mensajes_frame: u32,
    audio_frame: u32,
    video_seq: u16,
    video_partes: HashMap<u32, usize>,
    video_recoger: bool,
    pub entrada_activa: bool,
    entrada_frame: u32,
    respuesta_seq: u16,
    /// secuencias propias de lo que manda la depuración por canal,
    /// y cuántos paquetes más de cada canal se registran tras mandar algo.
    depurar_seq: HashMap<u32, u16>,
    /// pulsos dados (alternan el sentido del stick) y cuándo toca la
    /// trama de mando en reposo que cierra el pulso.
    pulsos: u32,
    reposo_en: Option<u64>,
    /// El mando tal como lo ve la consola (sus contadores siguen entre tramas)
    /// y el botón de depuración pulsado, que suelta `diferidas`.
    mando: Mando,
    pulsado: Option<Boton>,
    /// El botón que acaba de soltar `diferidas`, para el registro.
    pub soltado: Option<Boton>,
    vigilados: HashMap<u32, u32>,
    respuesta_frame: u32,
    qos: HashMap<u32, (Vec<u8>, HashSet<u32>, usize)>,
    /// Paquetes que no se conocen, por clase: cuántos llegaron.
    pub desconocidos: HashMap<String, u64>,
    pub chat_abierto: bool,
    chat_frame: u32,
    config_vista: bool,
    sondeos_vistos: u32,
    syn_consola: Option<usize>,
    espera_acuse: u32,
    ultimo_audio: Option<u32>,
    pub audio_paso: Option<u32>,
    /// Tramas de audio que no llegaron (huecos de frameId) y que llegaron fuera de orden.
    pub audio_perdidas: u64,
    pub audio_desordenadas: u64,
}

impl Protocolo {
    pub fn nuevo(opciones: Opciones) -> Self {
        Self {
            opciones, cliente_seq: 99, servidor_seq: 99, servidor_cambio: false, mtu: None, sondeos: 0,
            mensajes_seq: 0, mensajes_frame: 2_331_526_925, audio_frame: 42_537_843,
            video_seq: 0, video_partes: HashMap::new(), video_recoger: false,
            entrada_activa: false, entrada_frame: 0, respuesta_seq: 0, depurar_seq: HashMap::new(), pulsos: 0, reposo_en: None, mando: [0; 43], pulsado: None, soltado: None, vigilados: HashMap::new(), respuesta_frame: 2_134_489_808,
            qos: HashMap::new(), desconocidos: HashMap::new(), chat_abierto: false, chat_frame: 0, config_vista: false, sondeos_vistos: 0, syn_consola: None, espera_acuse: 0, ultimo_audio: None, audio_paso: None, audio_perdidas: 0, audio_desordenadas: 0,
        }
    }

    /// `sendPayload`: cabecera DCT con la confirmación (si llegó algo nuevo) y la
    /// secuencia del cliente.
    fn enviar(&mut self, ssrc: u32, pt: u8, carga: &[u8], ahora_us: u64) -> Salida {
        self.cliente_seq = self.cliente_seq.wrapping_add(1);
        let confirma = std::mem::take(&mut self.servidor_cambio).then_some(self.servidor_seq);
        Salida { pt, ssrc, carga: empaquetar_dct(carga, confirma, (ahora_us / 10_000) as u32, self.cliente_seq) }
    }

    /// Sin cabecera DCT (núcleo: sondeo, keepalive, URCP).
    fn crudo(pt: u8, carga: Vec<u8>) -> Salida { Salida { pt, ssrc: SSRC_NUCLEO, carga } }

    fn servidor(&mut self, h: &CabeceraDct) {
        // greenlight-native compara con «>», que se atasca al dar la vuelta a
        // 65535 (el audio la da en ~22 min): aquí, aritmética de serie.
        if let Some(s) = h.secuencia.filter(|s| *s != 0) {
            if (s.wrapping_sub(self.servidor_seq) as i16) > 0 { self.servidor_seq = s; self.servidor_cambio = true; }
        }
    }

    /// El siguiente sondeo de MTU, cada 10 ms, hasta que la consola conteste.
    pub fn sondeo(&mut self) -> Option<Salida> {
        if self.mtu.is_some() { return None }
        if self.sondeos >= SONDEOS {
            // Ninguna sonda nuestra acusada todavía: se espera `--espera-mtu`
            // (el acuse puede llegar después de sus primeros Syn, medido) y
            // luego se acepta el Syn de la consola.
            self.espera_acuse += 1;
            if self.espera_acuse <= self.opciones.espera_mtu_ms / 10 { return None }
            let n = self.syn_consola?;
            self.mtu = Some(n);
            return Some(Self::crudo(PT_SONDEO, sondeo_ack(n as u16)));
        }
        self.sondeos += 1;
        Some(Self::crudo(PT_SONDEO, sondeo_syn(1452 - 16 * self.sondeos as usize)))
    }

    /// La confirmación del núcleo, cada 50 ms, si llegó algo nuevo.
    pub fn confirmar(&mut self, ahora_us: u64) -> Option<Salida> {
        std::mem::take(&mut self.servidor_cambio).then(|| Salida { pt: PT_CANAL, ssrc: SSRC_NUCLEO, carga: confirmacion_nucleo(self.servidor_seq, (ahora_us / 10) as u32) })
    }

    /// Una trama de entrada, si el canal está abierto y el modo la manda.
    pub fn trama_entrada(&mut self, ahora_us: u64) -> Option<Salida> {
        if !self.entrada_activa || self.opciones.entrada == Entrada::Ninguna { return None }
        if self.opciones.entrada_hasta_us.is_some_and(|h| ahora_us >= h) { return None }
        self.entrada_frame = self.entrada_frame.wrapping_add(1);
        let mando = self.mando;
        let trama = trama_entrada(self.entrada_frame, ahora_us, (self.opciones.entrada == Entrada::Mando).then_some(&mando), self.opciones.entrada != Entrada::Vacia);
        Some(self.enviar(SSRC_ENTRADA, PT_CANAL, &canal(MUX_TRAMA, 0, 0, &trama), ahora_us))
    }

    /// Experimento del reto: silencio Opus por el canal ChatAudio (como unos
    /// auriculares con micrófono), una vez abierto. ¿Cuenta como «cliente
    /// conectado» sin ser entrada de juego? El formato es el de las tramas de
    /// audio de la consola; el tipo RTP, una suposición (39, el de su audio).
    pub fn trama_chat(&mut self, ahora_us: u64) -> Option<Salida> {
        // CELT de silencio (el paquete de 3 bytes que manda WebRTC en silencio).
        self.trama_chat_con(ahora_us, &[0xf8, 0xff, 0xfe])
    }

    /// Una trama Opus de 20 ms del micro por ChatAudio, con el
    /// mismo formato que las de audio de la consola.
    pub fn trama_chat_con(&mut self, ahora_us: u64, opus: &[u8]) -> Option<Salida> {
        if !self.chat_abierto { return None }
        self.chat_frame = self.chat_frame.wrapping_add(1);
        let trama = trama_audio(self.chat_frame, (ahora_us / 1000) as u32, opus);
        let pt = self.opciones.chat_pt;
        Some(self.enviar(SSRC_CHAT, pt, &trama, ahora_us))
    }

    /// Cuándo toca lo diferido (µs, el reloj de `ahora_us`), si hay algo.
    pub fn proxima_diferida(&self) -> Option<u64> { self.reposo_en }

    /// Lo diferido que ya toca mandar (la trama de reposo que cierra el pulso).
    pub fn diferidas(&mut self, ahora_us: u64) -> Vec<Salida> {
        if self.reposo_en.is_some_and(|t| ahora_us >= t) {
            self.reposo_en = None;
            self.soltado = self.pulsado;
            return vec![self.reposo(ahora_us)];
        }
        Vec::new()
    }

    /// un MuxDCTChannel Data al canal con `carga` tal cual.
    pub fn depurar_crudo(&mut self, ssrc: u32, carga: &[u8], ahora_us: u64) -> Salida { self.depurar_canal(ssrc, MUX_DATOS, carga, ahora_us) }

    /// un MuxDCTChannel Frame con su subtipo (3 = FrameAck).
    pub fn depurar_trama(&mut self, ssrc: u32, subtipo: u32, carga: &[u8], ahora_us: u64) -> Salida {
        let mut c = subtipo.to_le_bytes().to_vec();
        c.extend_from_slice(carga);
        self.depurar_canal(ssrc, MUX_TRAMA, &c, ahora_us)
    }

    fn depurar_canal(&mut self, ssrc: u32, tipo: u16, carga: &[u8], ahora_us: u64) -> Salida {
        let s = self.depurar_seq.entry(ssrc).or_insert(0);
        *s = s.wrapping_add(1);
        let s = *s;
        self.vigilados.insert(ssrc, 20);
        self.enviar(ssrc, PT_CANAL, &canal(tipo, s, s.wrapping_add(1), carga), ahora_us)
    }

    /// un mensaje de Messaging con el sobre clave/valor de siempre.
    pub fn depurar_mensaje(&mut self, ruta: &str, valor: &[u8], pide_ack: bool, ahora_us: u64) -> Salida {
        self.mensajes_seq = self.mensajes_seq.wrapping_add(1);
        self.mensajes_frame = self.mensajes_frame.wrapping_add(1);
        let s = self.mensajes_seq;
        self.vigilados.insert(SSRC_MENSAJES, 20);
        self.enviar(SSRC_MENSAJES, PT_CANAL, &canal(MUX_DATOS, s, s.wrapping_add(1), &datos_mensaje(if pide_ack { PIDE_ACK } else { SIN_ACK }, self.mensajes_frame, ruta, valor)), ahora_us)
    }

    /// El pulso ante `WarningForBeingIdle`: una trama con el stick un 10 %.
    ///
    /// el segundo aviso echaba la sesión. Las tramas normales no
    /// llevan mando, así que para la consola el mando se quedaba con el stick
    /// del primer pulso, y el segundo, idéntico, no era un cambio. Ahora cada
    /// pulso va al lado contrario que el anterior y 100 ms después sale una
    /// trama con el mando en reposo (por `diferidas`): dos cambios por aviso.
    pub fn pulso(&mut self, ahora_us: u64) -> Option<Salida> {
        if !self.entrada_activa { return None }
        self.pulsos = self.pulsos.wrapping_add(1);
        let eje: i16 = if self.pulsos % 2 == 1 { 3277 } else { -3277 };
        self.reposo_en = Some(ahora_us + 100_000);
        self.mando[MANDO_EJE_X..MANDO_EJE_X + 2].copy_from_slice(&eje.to_le_bytes());
        Some(self.trama_mando(ahora_us))
    }

    /// `boton` de depuración: pulsa en el acto y `diferidas` lo
    /// suelta a los `ms`. Si había otro pulsado, se suelta antes (el mando no
    /// se queda a medias). Sin canal Input (modo música), nada.
    pub fn boton(&mut self, cual: Boton, ms: u64, ahora_us: u64) -> Vec<Salida> {
        if !self.entrada_activa || self.opciones.entrada == Entrada::Ninguna { return Vec::new() }
        let mut sal = Vec::new();
        if self.pulsado.is_some() { sal.push(self.reposo(ahora_us)) }
        let (contador, mascara) = cual.bits();
        self.mando[contador] = self.mando[contador].wrapping_add(1);
        self.mando[MANDO_BOTONES..MANDO_BOTONES + 2].copy_from_slice(&mascara.to_le_bytes());
        self.pulsado = Some(cual);
        self.reposo_en = Some(ahora_us + ms * 1000);
        sal.push(self.trama_mando(ahora_us));
        sal
    }

    /// A18: el mando emulado con el ratón y el teclado del PC. Manda una trama
    /// sólo si algo cambió. Sin canal Input (modo música), nada.
    pub fn mando_pc(&mut self, traductor: &mut crate::raton_teclado::Traductor, e: &crate::raton_teclado::Estado, ahora_us: u64) -> Option<Salida> {
        if !self.entrada_activa { return None }
        traductor.aplicar(e, &mut self.mando).then(|| self.trama_mando(ahora_us))
    }

    /// El mando en reposo: ejes a cero y, si había un botón pulsado, suelto
    /// (su contador sube otra vez, como greenlight-native).
    fn reposo(&mut self, ahora_us: u64) -> Salida {
        if let Some(b) = self.pulsado.take() { let (c, _) = b.bits(); self.mando[c] = self.mando[c].wrapping_add(1) }
        self.mando[MANDO_EJE_X..MANDO_EJE_X + 8].fill(0);
        self.mando[MANDO_BOTONES..MANDO_BOTONES + 2].fill(0);
        self.trama_mando(ahora_us)
    }

    fn trama_mando(&mut self, ahora_us: u64) -> Salida {
        self.entrada_frame = self.entrada_frame.wrapping_add(1);
        let trama = trama_mando(self.entrada_frame, ahora_us, &self.mando);
        self.enviar(SSRC_ENTRADA, PT_CANAL, &canal(MUX_TRAMA, 0, 0, &trama), ahora_us)
    }

    /// Para cerrar: UDPKeepAlive Disconnect.
    pub fn despedida() -> Salida { Self::crudo(PT_KEEPALIVE, keepalive(0, KA_DESCONEXION)) }

    fn desconocido(&mut self, clave: String, datos: &[u8], ev: &mut Vec<Evento>) {
        let n = self.desconocidos.entry(clave.clone()).or_insert(0);
        *n += 1;
        // Los tres primeros de cada clase, enteros hasta 256 bytes (tipos 5 y 175).
        if *n <= 3 {
            let hex: String = datos.iter().take(256).map(|b| format!("{b:02x}")).collect();
            ev.push(Evento::Nota(format!("desconocido: {clave} ({} B) {hex}", datos.len())));
        }
    }

    pub fn recibir(&mut self, pt: u8, ssrc: u32, carga: &[u8], ahora_us: u64) -> (Vec<Salida>, Vec<Evento>) {
        let (mut sal, mut ev) = (Vec::new(), Vec::new());
        if let Some(n) = self.vigilados.get_mut(&ssrc).filter(|n| **n > 0) {
            *n -= 1;
            ev.push(Evento::Nota(format!("depuración ← {} (ssrc {ssrc}, tipo RTP {pt}, {} B): {}", nombre_canal(ssrc), carga.len(),
                carga.iter().take(64).map(|b| format!("{b:02x}")).collect::<String>())));
        }
        match pt {
            PT_SONDEO => {
                // Los primeros sondeos de la consola, para entender el MTU (encargo: ¿por qué 1232?).
                self.sondeos_vistos += 1;
                if self.sondeos_vistos <= 6 {
                    let que = match leer_sondeo(carga) { Some(Sondeo::Syn(n)) => format!("Syn {n}"), Some(Sondeo::Ack(n)) => format!("Ack {n}"), None => "ilegible".into() };
                    ev.push(Evento::Nota(format!("sondeo de la consola #{}: {que} ({} B)", self.sondeos_vistos, carga.len())));
                }
                // El MTU es el de NUESTRA sonda más grande que la consola acusa (su
                // Ack), como le salía a greenlight-native (1434). Si se confirma su
                // Syn de 1232 en cuanto llega, la consola se queda en 1232 y manda
                // el Config con el bit 0x20 (medido el 01-10). Su Syn sólo vale si
                // no acusa ninguna sonda nuestra (`sondeo()` agota las 16).
                match leer_sondeo(carga) {
                    Some(Sondeo::Ack(n)) if self.mtu.is_none() => {
                        let mtu = usize::from(n);
                        self.mtu = Some(mtu);
                        sal.push(Self::crudo(PT_SONDEO, sondeo_ack(n)));
                        ev.push(Evento::Nota(format!("MTU acordado: {mtu} (acuse de nuestra sonda)")));
                    }
                    Some(Sondeo::Syn(n)) if self.mtu.is_none() && self.opciones.mtu_en_el_acto => {
                        self.mtu = Some(n);
                        sal.push(Self::crudo(PT_SONDEO, sondeo_ack(n as u16)));
                        ev.push(Evento::Nota(format!("MTU acordado: {n} (su primer Syn, acusado en el acto)")));
                    }
                    Some(Sondeo::Syn(n)) if self.mtu.is_none() => { self.syn_consola = Some(self.syn_consola.unwrap_or(0).max(n)); }
                    _ => {}
                }
            },
            // La consola de 2026 puede poner el bit 0x20 en el tipo (Config = 41,
            // con el MTU en un u32 al final; medido el 01-10): se lee sin él.
            PT_KEEPALIVE => match leer_keepalive(carga).map(|(c, t)| (c, t & !0x20)) {
                Some((_, KA_CONFIG)) => {
                    if !self.config_vista {
                        self.config_vista = true;
                        let t = leer_keepalive(carga).map(|k| k.1).unwrap_or(0);
                        ev.push(Evento::Nota(format!("Config del núcleo (tipo {t}, {} B); se contesta con ConfigAck tipo {KA_CONFIG_ACK}", carga.len())));
                    }
                    sal.push(Self::crudo(PT_URCP, urcp_config()));
                    sal.push(Self::crudo(PT_KEEPALIVE, keepalive(40, KA_CONFIG_ACK)));
                }
                Some((_, KA_DESCONEXION)) => ev.push(Evento::Desconexion),
                Some((_, KA_NINGUNO)) => {
                    let mut c = keepalive(1382, KA_NINGUNO);
                    c.resize(self.mtu.unwrap_or(0).max(c.len()), 0);
                    sal.push(Self::crudo(PT_KEEPALIVE, c));
                }
                Some((_, KA_CONFIG_ACK)) => {}
                _ => self.desconocido(format!("keepalive {:?}", leer_keepalive(carga).map(|k| k.1)), carga, &mut ev),
            },
            PT_URCP => match leer_urcp(carga) {
                Some(0) => { sal.push(Self::crudo(PT_URCP, urcp_aceptado())); ev.push(Evento::Nota("núcleo negociado".into())); }
                Some(1 | 2) => {}
                t => self.desconocido(format!("URCP {t:?}"), carga, &mut ev),
            },
            PT_CONTROL => {
                let Some((h, cuerpo, _)) = leer_dct(carga) else { self.desconocido("cabecera DCT ilegible (control)".into(), carga, &mut ev); return (sal, ev) };
                self.servidor(&h);
                match leer_control(cuerpo) {
                    Some(Control::Abrir { nombre }) => {
                        let nombre = nombre.rsplit("::").next().unwrap_or(nombre).to_owned();
                        ev.push(Evento::Nota(format!("la consola abre el canal {nombre} (ssrc {ssrc})")));
                        let datos: &[u8] = if ssrc == SSRC_CONTROL { &[3, 0] } else { &[] };
                        let s = self.enviar(ssrc, PT_CONTROL, &control_confirmar(datos), ahora_us);
                        sal.push(s);
                        match ssrc {
                            SSRC_CONTROL => { let s = self.enviar(ssrc, 35, &canal(MUX_ABRIR, 0, 0, &abrir_peticion_udid(UDID)), ahora_us); sal.push(s) }
                            SSRC_CHAT => { self.chat_abierto = true; let s = self.enviar(ssrc, 36, &canal(MUX_ABRIR, 0, 0, &abrir_chat(ahora_us)), ahora_us); sal.push(s) }
                            SSRC_RESPUESTA => {
                                self.respuesta_frame = self.respuesta_frame.wrapping_add(1);
                                let s = self.enviar(ssrc, 35, &canal(MUX_CONFIG, 0, 0, &config_entrada(self.respuesta_frame)), ahora_us);
                                sal.push(s);
                            }
                            _ => {}
                        }
                    }
                    Some(Control::Confirmar) => {}
                    Some(Control::Cerrar) => ev.push(Evento::CanalCerrado(ssrc)),
                    None => self.desconocido(format!("MuxDCTControl ssrc {ssrc}"), cuerpo, &mut ev),
                }
            }
            35..=63 => {
                let Some((h, cuerpo, _)) = leer_dct(carga) else { self.desconocido(format!("cabecera DCT ilegible (ssrc {ssrc})"), carga, &mut ev); return (sal, ev) };
                self.servidor(&h);
                let Some((tipo, datos)) = leer_canal(cuerpo) else { self.desconocido(format!("MuxDCTChannel ilegible ssrc {ssrc}"), cuerpo, &mut ev); return (sal, ev) };
                self.canal(ssrc, tipo, datos, ahora_us, &mut sal, &mut ev);
            }
            _ => self.desconocido(format!("tipo RTP {pt} ssrc {ssrc}"), carga, &mut ev),
        }
        (sal, ev)
    }

    fn canal(&mut self, ssrc: u32, tipo: u16, datos: &[u8], ahora_us: u64, sal: &mut Vec<Salida>, ev: &mut Vec<Evento>) {
        let subtipo16 = Lector::nuevo(datos).u16();
        let subtipo32 = Lector::nuevo(datos).u32();
        match (ssrc, tipo) {
            (_, MUX_NINGUNO) if datos.is_empty() => {}
            (SSRC_AUDIO, MUX_NINGUNO) => match leer_audio(datos) {
                Some(a) => {
                    // ¿Pérdida en la red o retraso? Por los huecos de frameId (consecutivos).
                    match self.ultimo_audio {
                        Some(u) if (a.frame_id.wrapping_sub(u) as i32) > 0 => {
                            // El paso entre tramas seguidas se aprende (el menor salto visto):
                            // no se sabe si el frameId sube de 1 en 1.
                            let salto = a.frame_id.wrapping_sub(u);
                            let paso = *self.audio_paso.get_or_insert(salto);
                            if salto < paso { self.audio_paso = Some(salto) }
                            let paso = self.audio_paso.unwrap_or(1).max(1);
                            if salto / paso < 100 { self.audio_perdidas += u64::from(salto / paso).saturating_sub(1) }
                            self.ultimo_audio = Some(a.frame_id);
                        }
                        Some(_) => self.audio_desordenadas += 1,
                        None => self.ultimo_audio = Some(a.frame_id),
                    }
                    ev.push(Evento::Audio { frame_id: a.frame_id, opus: a.opus.to_vec() })
                }
                None => self.desconocido("audio ilegible".into(), datos, ev),
            },
            (SSRC_CONTROL, MUX_ABRIR) if matches!(leer_abrir(datos), Some(Abrir::Respuesta)) => {
                ev.push(Evento::Nota("canal Control listo".into()));
                let s = self.enviar(ssrc, 35, &canal(MUX_DATOS, 0, 1, &datos_control()), ahora_us);
                sal.push(s);
            }
            (SSRC_QOS, MUX_ABRIR) => { let s = self.enviar(ssrc, 37, &canal(MUX_ABRIR, 0, 1, &abrir_respuesta()), ahora_us); sal.push(s) }
            (SSRC_QOS, MUX_DATOS) if subtipo16 == Some(DATOS_VARIOS) => {
                let Some(p) = leer_parte(&datos[2..]) else { return };
                // El total viene de la red: con cota (revisión 19 §2.4; la política real
                // ocupa unos cientos de bytes) y pocas a la vez, sin alojar a ciegas.
                if p.total > QOS_MAXIMO || (self.qos.len() >= 4 && !self.qos.contains_key(&p.total)) {
                    ev.push(Evento::Nota(format!("política de QoS descartada: {} B anunciados", p.total)));
                    return
                }
                let (buf, vistos, escritos) = self.qos.entry(p.total).or_insert_with(|| (vec![0; p.total as usize], HashSet::new(), 0));
                if vistos.insert(p.desplazamiento) {
                    let ini = (p.desplazamiento as usize).min(buf.len());
                    let n = p.datos.len().min(buf.len() - ini);
                    buf[ini..ini + n].copy_from_slice(&p.datos[..n]);
                    *escritos += n;
                }
                if *escritos >= p.total as usize {
                    ev.push(Evento::Nota(format!("política de QoS recibida ({} B)", p.total)));
                    self.qos.remove(&p.total);
                    let s = self.enviar(ssrc, 41, &canal(MUX_DATOS, 0, 1, &datos_varios_vacio()), ahora_us);
                    sal.push(s);
                }
            }
            (SSRC_VIDEO, MUX_CONFIG) if subtipo16 == Some(CONFIG_VIDEO_SERVIDOR) => {
                ev.push(Evento::Nota("vídeo configurado: canales listos".into()));
                let (fps, ancho, alto) = self.opciones.video;
                for (s, c) in [
                    (SSRC_CONTROL, canal(MUX_DATOS, 1, 2, &datos_trama_vacia())),
                    (SSRC_VIDEO, canal(MUX_CONFIG, 0, 0, &config_video_cliente(823_738_637, fps, ancho, alto))),
                    (SSRC_VIDEO, canal(MUX_CONFIG_ACK, 0, 1, &config_ack_video())),
                ] { let x = self.enviar(s, 35, &c, ahora_us); sal.push(x) }
            }
            (SSRC_VIDEO, MUX_TRAMA) if subtipo32 == Some(TRAMA_VIDEO) => self.video(&datos[4..], ahora_us, sal, ev),
            (SSRC_VIDEO, MUX_CONFIG_ACK) if subtipo16 == Some(4) => self.video(datos.get(4..).unwrap_or(&[]), ahora_us, sal, ev),
            (SSRC_AUDIO, MUX_ABRIR) if matches!(leer_abrir(datos), Some(Abrir::Audio)) => {
                self.audio_frame = self.audio_frame.wrapping_add(1);
                ev.push(Evento::Nota("canal Audio abierto".into()));
                let a = self.enviar(ssrc, 37, &canal(MUX_ABRIR, 0, 0, &abrir_audio_ack(self.audio_frame, 0)), ahora_us);
                let b = self.enviar(ssrc, 38, &canal(MUX_DATOS, 0, 1, &datos_ack()), ahora_us);
                sal.extend([a, b]);
            }
            (SSRC_MENSAJES, MUX_ABRIR) if matches!(leer_abrir(datos), Some(Abrir::Peticion)) => self.abrir_mensajes(ahora_us, sal),
            (SSRC_MENSAJES, MUX_DATOS) if subtipo16 == Some(DATOS_MENSAJE) => match leer_mensaje(&datos[2..]) {
                Some(m) => {
                    if !m.clave.is_empty() { ev.push(Evento::Mensaje { clave: m.clave.to_owned(), valor: String::from_utf8_lossy(m.valor).into_owned() }) }
                    if m.ack == PIDE_ACK {
                        self.mensajes_seq = self.mensajes_seq.wrapping_add(1);
                        let s = self.mensajes_seq;
                        let x = self.enviar(ssrc, 35, &canal(MUX_DATOS, s, s.wrapping_add(1), &datos_mensaje(ES_ACK, m.frame_id, "", b"")), ahora_us);
                        sal.push(x);
                    }
                }
                None => self.desconocido("mensaje ilegible".into(), datos, ev),
            },
            (SSRC_ENTRADA, MUX_CONFIG) if subtipo16 == Some(CONFIG_ENTRADA) => {
                self.entrada_frame = leer_config_entrada(&datos[2..]).unwrap_or(0);
                ev.push(Evento::Nota(format!("canal Input abierto; tramas de entrada: {:?}", self.opciones.entrada)));
                let s = self.enviar(ssrc, 35, &canal(MUX_CONFIG, 0, 0, &config_entrada_ack(ahora_us)), ahora_us);
                sal.push(s);
                self.entrada_activa = true;
            }
            (SSRC_RESPUESTA, MUX_TRAMA) if subtipo32 == Some(TRAMA_ENTRADA) => {
                let frame_id = Lector::nuevo(datos.get(8..).unwrap_or(&[])).u32().unwrap_or(0);
                let s = self.respuesta_seq;
                self.respuesta_seq = s.wrapping_add(1);
                let x = self.enviar(ssrc, 35, &canal(MUX_TRAMA, s, s.wrapping_add(1), &trama_ack(frame_id)), ahora_us);
                sal.push(x);
            }
            (SSRC_CONTROL, MUX_DATOS) if subtipo16 == Some(DATOS_CONTROL_SERVIDOR) => ev.push(Evento::Nota("Control: Data subtipo 5 de la consola".into())),
            // Acuses de la consola (medido el 01-10 con la consola de 2026): de nuestras
            // tramas de entrada (Frame Ack con su frameId, ~83/s), su InputAck por
            // InputFeedback, y Data/Data de vídeo.
            (SSRC_ENTRADA, MUX_TRAMA) if subtipo32 == Some(TRAMA_ACK) => {}
            (SSRC_RESPUESTA, MUX_CONFIG) if subtipo16 == Some(6) => {}
            (SSRC_VIDEO, MUX_DATOS) if subtipo16 == Some(DATOS_DATOS) => {}
            // Lo que se sabe que llega y no pide respuesta.
            (SSRC_CONTROL | SSRC_QOS | SSRC_VIDEO | SSRC_AUDIO | SSRC_CHAT | SSRC_ENTRADA, MUX_DATOS) if subtipo16 == Some(DATOS_ACK) => {}
            (SSRC_CHAT, MUX_ABRIR) => {
                let clase = match leer_abrir(datos) { Some(Abrir::AudioAck) => "AudioAck".to_owned(), Some(Abrir::Audio) => "Audio".into(), _ => format!("{subtipo16:?}") };
                ev.push(Evento::Nota(format!("ChatAudio: la consola responde {clase} {}", datos.iter().take(40).map(|b| format!("{b:02x}")).collect::<String>())));
            }
            (SSRC_CHAT, _) => {}
            _ => {
                let sub = match tipo { MUX_TRAMA => subtipo32.map(i64::from), _ => subtipo16.map(i64::from) };
                self.desconocido(format!("MuxDCTChannel ssrc {ssrc} tipo {tipo} subtipo {sub:?}"), datos, ev);
            }
        }
    }

    fn abrir_mensajes(&mut self, ahora_us: u64, sal: &mut Vec<Salida>) {
        let a = self.enviar(SSRC_MENSAJES, 35, &canal(MUX_ABRIR, 0, 0, &abrir_peticion_mensajes()), ahora_us);
        self.mensajes_frame = self.mensajes_frame.wrapping_add(1);
        let b = self.enviar(SSRC_MENSAJES, 35, &canal(MUX_DATOS, 0, 1, &datos_mensaje(SIN_ACK, self.mensajes_frame, "/streaming/systemUi/configuration", br#"{"systemUis":[],"version":[0,2,0]}"#)), ahora_us);
        sal.extend([a, b]);
        let party = if self.opciones.party_activa { r#"{"partyChatActive":true}"# } else { r#"{"partyChatActive":false}"# };
        let claves: [(&str, &str, u32); 7] = [
            ("/streaming/characteristics/dimensionschanged", r#"{"horizontal":130,"preferredHeight":1170,"preferredWidth":2532,"safeAreaBottom":1107,"safeAreaLeft":141,"safeAreaRight":2391,"safeAreaTop":0,"supportsCustomResolution":false,"vertical":73}"#, SIN_ACK),
            ("/streaming/characteristics/orientationchanged", r#"{"orientation":0}"#, SIN_ACK),
            ("/streaming/social/partyChatAudioCoordination/setPartyChatActive", party, PIDE_ACK),
            ("/streaming/properties/clientappinstallidchanged", r#"{"clientAppInstallId":"66354ea190f8a47031fff981236fac55"}"#, SIN_ACK),
            ("/streaming/characteristics/touchinputenabledchanged", r#"{"touchInputEnabled":false}"#, SIN_ACK),
            ("/streaming/characteristics/touchBundleMetadataChanged", "null", SIN_ACK),
            ("/streaming/characteristics/clientdevicecapabilities", r#"{"maxTouchBundleLayoutVersion":"3.4.1.0","maxTouchBundleManifestVersion":"2.0.0.0"}"#, PIDE_ACK),
        ];
        for (clave, valor, ack) in claves {
            self.mensajes_seq = self.mensajes_seq.wrapping_add(1);
            self.mensajes_frame = self.mensajes_frame.wrapping_add(1);
            let s = self.mensajes_seq;
            let x = self.enviar(SSRC_MENSAJES, 35, &canal(MUX_DATOS, s, s.wrapping_add(1), &datos_mensaje(ack, self.mensajes_frame, clave, valor.as_bytes())), ahora_us);
            sal.push(x);
        }
    }

    /// `processVideoData`: no se guarda nada; sólo se confirma cada trama completa
    /// y, con la primera grande, el núcleo dice «canales listos».
    fn video(&mut self, datos: &[u8], ahora_us: u64, sal: &mut Vec<Salida>, ev: &mut Vec<Evento>) {
        let Some(v) = leer_video(datos) else { self.desconocido("trama de vídeo ilegible".into(), datos, ev); return };
        let completa = if v.paquetes > 1 {
            let escritos = self.video_partes.entry(v.frame_id).or_insert(0);
            *escritos += v.datos;
            if *escritos >= v.total as usize {
                self.video_partes.remove(&v.frame_id);
                if !self.video_recoger && v.unknown2 > 5 {
                    self.video_recoger = true;
                    ev.push(Evento::Nota("primera trama de vídeo completa: canales listos (núcleo)".into()));
                    sal.push(Self::crudo(PT_KEEPALIVE, keepalive(1362, KA_NINGUNO)));
                    sal.push(Self::crudo(PT_KEEPALIVE, keepalive(288, KA_NINGUNO)));
                }
                true
            } else { false }
        } else { self.video_recoger };
        // Tramas que nunca se completan: que el mapa no crezca sin fin.
        if self.video_partes.len() > 64 { self.video_partes.clear() }
        if completa {
            self.video_seq = self.video_seq.wrapping_add(1);
            let s = self.video_seq;
            let x = self.enviar(SSRC_VIDEO, 35, &canal(MUX_DATOS, s, s.wrapping_add(1), &datos_video_ack(v.frame_id, (ahora_us / 1000) as u32)), ahora_us);
            sal.push(x);
        }
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn control_abrir(nombre: &str) -> Vec<u8> {
        let cuerpo = Escritor::default().u32(2).u16(nombre.len() as u16).bytes(nombre.as_bytes()).u32(0).u32(0).u16(0).fin();
        empaquetar_dct(&cuerpo, None, 0, 150)
    }
    fn canal_srv(tipo: u16, datos: &[u8], seq: u16) -> Vec<u8> { empaquetar_dct(&canal(tipo, 0, 0, datos), None, 0, seq) }

    #[test]
    fn nucleo_y_mtu() {
        let mut p = Protocolo::nuevo(Opciones::default());
        let primero = p.sondeo().unwrap();
        assert_eq!((primero.pt, primero.carga.len()), (PT_SONDEO, 1436 + 2));
        // Su Syn de 1232 no se confirma: se espera al acuse de nuestra sonda.
        let (sal, _) = p.recibir(PT_SONDEO, 0, &sondeo_syn(1232), 0);
        assert!(sal.is_empty());
        let (sal, _) = p.recibir(PT_SONDEO, 0, &sondeo_ack(1436), 0);
        assert_eq!(sal, vec![Salida { pt: PT_SONDEO, ssrc: 0, carga: sondeo_ack(1436) }]);
        assert_eq!((p.sondeo(), p.mtu), (None, Some(1436)));
        // Sin acuse en las 16 sondas: vale su Syn.
        let mut q = Protocolo::nuevo(Opciones::default());
        q.recibir(PT_SONDEO, 0, &sondeo_syn(1232), 0);
        let ultima = (0..20).filter_map(|_| q.sondeo()).last().unwrap();
        assert_eq!((ultima.carga, q.mtu), (sondeo_ack(1232), Some(1232)));
        let (sal, _) = p.recibir(PT_KEEPALIVE, 0, &keepalive(0, KA_CONFIG), 0);
        assert_eq!(sal.iter().map(|s| s.pt).collect::<Vec<_>>(), vec![PT_URCP, PT_KEEPALIVE]);
        // La de 2026: tipo 41 (9 | 0x20) y el MTU al final.
        let mut nueva = Escritor::default().u32(0).u32(41).fin();
        nueva.extend_from_slice(&keepalive(0, KA_CONFIG)[8..]);
        let (sal, _) = p.recibir(PT_KEEPALIVE, 0, &Escritor::default().bytes(&nueva).u32(1232).fin(), 0);
        assert_eq!(sal.iter().map(|s| s.pt).collect::<Vec<_>>(), vec![PT_URCP, PT_KEEPALIVE]);
        let (sal, _) = p.recibir(PT_KEEPALIVE, 0, &keepalive(5, KA_NINGUNO), 0);
        assert_eq!(sal[0].carga.len(), 1436);
        let (_, ev) = p.recibir(PT_KEEPALIVE, 0, &keepalive(0, KA_DESCONEXION), 0);
        assert_eq!(ev, vec![Evento::Desconexion]);
    }

    #[test]
    fn canales_y_audio() {
        let mut p = Protocolo::nuevo(Opciones::default());
        let (sal, _) = p.recibir(PT_CONTROL, SSRC_CONTROL, &control_abrir("Microsoft::Basix::Dct::Channel::Class::Control"), 0);
        assert_eq!(sal.len(), 2);
        // La confirmación lleva la secuencia del servidor (150) y la del cliente (100).
        let (h, cuerpo, _) = leer_dct(&sal[0].carga).unwrap();
        assert_eq!((h.confirma, h.secuencia, &cuerpo[..4]), (Some(150), Some(100), &[3u8, 0, 0, 0][..]));
        // Audio: abrir y recibir una trama.
        p.recibir(PT_CONTROL, SSRC_AUDIO, &control_abrir("Microsoft::Basix::Dct::Channel::Class::Audio"), 0);
        let abrir = Escritor::default().u16(7).u16(0).u64(1).u32(1).u32(2).u32(48_000).u32(0).fin();
        let (sal, _) = p.recibir(36, SSRC_AUDIO, &canal_srv(MUX_ABRIR, &abrir, 151), 0);
        assert_eq!(sal.iter().map(|s| s.pt).collect::<Vec<_>>(), vec![37, 38]);
        let trama = Escritor::default().u16(0).u32(4).u32(77).u32(0).u32(0).u32(3).bytes(&[0xfc, 1, 2]).fin();
        let cuerpo = Escritor::default().u16(0).u32(0).bytes(&trama).u16(0).fin();
        let (_, ev) = p.recibir(39, SSRC_AUDIO, &empaquetar_dct(&cuerpo, None, 0, 152), 0);
        assert_eq!(ev, vec![Evento::Audio { frame_id: 77, opus: vec![0xfc, 1, 2] }]);
        // Paso 1 (77 → 78), un hueco de dos (79 y 80) y una que llega tarde (79).
        for (id, seq) in [(78u32, 153u16), (81, 154), (79, 155)] {
            let trama = Escritor::default().u16(0).u32(4).u32(id).u32(0).u32(0).u32(1).bytes(&[0xfc]).fin();
            let cuerpo = Escritor::default().u16(0).u32(0).bytes(&trama).u16(0).fin();
            p.recibir(39, SSRC_AUDIO, &empaquetar_dct(&cuerpo, None, 0, seq), 0);
        }
        assert_eq!((p.audio_perdidas, p.audio_desordenadas), (2, 1));
        // Llegó algo nuevo: hay confirmación del núcleo, y sólo una.
        assert!(p.confirmar(0).is_some());
        assert!(p.confirmar(0).is_none());
    }

    #[test]
    fn entrada_segun_modo() {
        for (entrada, hasta, esperado) in [(Entrada::Estadisticas, None, true), (Entrada::Ninguna, None, false), (Entrada::Estadisticas, Some(5), false)] {
            let mut p = Protocolo::nuevo(Opciones { entrada, entrada_hasta_us: hasta, ..Default::default() });
            assert!(p.trama_entrada(10).is_none());
            let config = Escritor::default().u16(CONFIG_ENTRADA).u16(0).u32(24).u32(4).u32(11).u32(1280).u32(720).u32(0).u32(500).fin();
            let (sal, _) = p.recibir(35, SSRC_ENTRADA, &canal_srv(MUX_CONFIG, &config, 3), 0);
            assert_eq!(sal.len(), 1);
            assert_eq!(p.trama_entrada(10).is_some(), esperado);
        }
    }

    /// dos avisos seguidos. Cada pulso mueve el stick al lado
    /// contrario que el anterior, y 100 ms después sale el mando en reposo.
    #[test]
    fn cada_pulso_es_un_cambio() {
        let mut p = Protocolo::nuevo(Opciones::default());
        assert!(p.pulso(0).is_none());
        let config = Escritor::default().u16(CONFIG_ENTRADA).u16(0).u32(24).u32(4).u32(11).u32(1280).u32(720).u32(0).u32(500).fin();
        p.recibir(35, SSRC_ENTRADA, &canal_srv(MUX_CONFIG, &config, 3), 0);
        // El eje X del stick izquierdo de una trama de mando (en su cuerpo DCT).
        let eje = |s: &Salida| { let (_, c, _) = leer_dct(&s.carga).unwrap(); let m = 8 + 4 + 4 + 4 + 8 + 2 + 2; i16::from_le_bytes([c[m + 19], c[m + 20]]) };
        let mut ejes = Vec::new();
        for t in [0u64, 600_000_000] {
            ejes.push(eje(&p.pulso(t).unwrap()));
            assert!(p.diferidas(t + 50_000).is_empty());
            let r = p.diferidas(t + 100_000);
            assert_eq!(r.len(), 1);
            ejes.push(eje(&r[0]));
        }
        assert_eq!(ejes, vec![3277, 0, -3277, 0]);
    }

    /// `boton` pulsa en el acto y `diferidas` lo suelta a los ms
    /// pedidos; cada cambio sube el contador del botón; otro botón a medias
    /// suelta antes el anterior; sin canal Input, nada.
    #[test]
    fn boton_de_depuracion() {
        let mut p = Protocolo::nuevo(Opciones::default());
        assert!(p.boton(Boton::A, 100, 0).is_empty(), "sin canal Input");
        let config = Escritor::default().u16(CONFIG_ENTRADA).u16(0).u32(24).u32(4).u32(11).u32(1280).u32(720).u32(0).u32(500).fin();
        p.recibir(35, SSRC_ENTRADA, &canal_srv(MUX_CONFIG, &config, 3), 0);
        // (contador del botón, máscara) de una trama de mando.
        let leer = |s: &Salida, contador: usize| { let (_, c, _) = leer_dct(&s.carga).unwrap(); let m = 8 + 4 + 4 + 4 + 8 + 2 + 2; (c[m + contador], u16::from_le_bytes([c[m + 35], c[m + 36]])) };
        let r = p.boton(Boton::A, 100, 1_000_000);
        assert_eq!(r.len(), 1);
        assert_eq!(leer(&r[0], 13), (1, 0x1000));
        assert!(p.diferidas(1_050_000).is_empty());
        let r = p.diferidas(1_100_000);
        assert_eq!((leer(&r[0], 13), p.soltado.take()), ((2, 0), Some(Boton::A)));
        // Arriba y, a medias, abajo: primero se suelta arriba.
        p.boton(Boton::Arriba, 100, 2_000_000);
        let r = p.boton(Boton::Abajo, 100, 2_020_000);
        assert_eq!((r.len(), leer(&r[0], 1), leer(&r[1], 2)), (2, (2, 0), (1, 0x0002)));
        assert_eq!(leer(&p.diferidas(2_120_000)[0], 2), (2, 0));
        // En modo música (sin entrada), nada.
        let mut m = Protocolo::nuevo(Opciones { entrada: Entrada::Ninguna, ..Default::default() });
        m.recibir(35, SSRC_ENTRADA, &canal_srv(MUX_CONFIG, &config, 3), 0);
        assert!(m.boton(Boton::A, 100, 0).is_empty());
        assert_eq!(Boton::de("izquierda"), Some(Boton::Izquierda));
        assert_eq!(Boton::de("x"), None);
    }

    #[test]
    fn mtu_en_el_acto() {
        let mut p = Protocolo::nuevo(Opciones { mtu_en_el_acto: true, ..Default::default() });
        let (sal, _) = p.recibir(PT_SONDEO, 0, &sondeo_syn(1232), 0);
        assert_eq!(sal, vec![Salida { pt: PT_SONDEO, ssrc: 0, carga: sondeo_ack(1232) }]);
        assert_eq!((p.mtu, p.sondeo()), (Some(1232), None));
        // Su siguiente Syn ya no se contesta.
        assert!(p.recibir(PT_SONDEO, 0, &sondeo_syn(1216), 0).0.is_empty());
    }

    #[test]
    fn chat_con_voz_y_party() {
        let mut p = Protocolo::nuevo(Opciones { party_activa: true, chat_pt: 36, ..Default::default() });
        assert!(p.trama_chat_con(0, &[1, 2, 3]).is_none());
        let (sal, _) = p.recibir(PT_CONTROL, SSRC_CHAT, &control_abrir("Microsoft::Basix::Dct::Channel::Class::ChatAudio"), 0);
        assert!(!sal.is_empty());
        let s = p.trama_chat_con(5_000, &[0xaa, 0xbb]).unwrap();
        assert_eq!((s.pt, s.ssrc), (36, SSRC_CHAT));
        let (_, cuerpo, _) = leer_dct(&s.carga).unwrap();
        let t = trama_audio(1, 5, &[0xaa, 0xbb]);
        assert_eq!(cuerpo, &t[..t.len() - 2]);
        // setPartyChatActive true al abrir Messaging.
        let (sal, _) = p.recibir(35, SSRC_MENSAJES, &canal_srv(MUX_ABRIR, &abrir_peticion_mensajes(), 4), 0);
        let aguja = br#""partyChatActive":true"#;
        assert!(sal.iter().any(|s| s.carga.windows(aguja.len()).any(|w| w == aguja)));
    }

    #[test]
    fn qos_con_cota() {
        let mut p = Protocolo::nuevo(Opciones::default());
        let parte = |total: u32| Escritor::default().u16(DATOS_VARIOS).u16(0).u32(total).u32(1).u32(0).u32(3).bytes(&[1, 2, 3]).fin();
        // Un total enorme (o u32::MAX) no aloja nada: se descarta con una nota.
        let (_, ev) = p.recibir(35, SSRC_QOS, &canal_srv(MUX_DATOS, &parte(u32::MAX), 6), 0);
        assert!(matches!(&ev[..], [Evento::Nota(n)] if n.contains("descartada")));
        assert!(p.qos.is_empty());
        // La de verdad, entera en una parte: se confirma.
        let (sal, ev) = p.recibir(35, SSRC_QOS, &canal_srv(MUX_DATOS, &parte(3), 7), 0);
        assert!(matches!(&ev[..], [Evento::Nota(n)] if n.contains("recibida (3 B)")));
        assert_eq!(sal.len(), 1);
    }

    #[test]
    fn mensajes_y_desconocidos() {
        let mut p = Protocolo::nuevo(Opciones::default());
        let (sal, _) = p.recibir(35, SSRC_MENSAJES, &canal_srv(MUX_ABRIR, &abrir_peticion_mensajes(), 4), 0);
        assert_eq!(sal.len(), 2 + 7);
        let m = datos_mensaje(PIDE_ACK, 9, "/streaming/titleinfo", b"{}");
        let (sal, ev) = p.recibir(35, SSRC_MENSAJES, &canal_srv(MUX_DATOS, &m, 5), 0);
        assert_eq!(sal.len(), 1);
        assert_eq!(ev, vec![Evento::Mensaje { clave: "/streaming/titleinfo".into(), valor: "{}".into() }]);
        // Un subtipo de Data que no se conoce: se anota una vez.
        let raro = Escritor::default().u16(6).u32(1).fin();
        let (_, ev) = p.recibir(35, SSRC_CONTROL, &canal_srv(MUX_DATOS, &raro, 6), 0);
        assert_eq!(ev.len(), 1);
        for seq in 7..10 { p.recibir(35, SSRC_CONTROL, &canal_srv(MUX_DATOS, &raro, seq), 0); }
        let (_, ev) = p.recibir(35, SSRC_CONTROL, &canal_srv(MUX_DATOS, &raro, 10), 0);
        assert!(ev.is_empty());
        assert_eq!(p.desconocidos.values().sum::<u64>(), 5);
    }
}
