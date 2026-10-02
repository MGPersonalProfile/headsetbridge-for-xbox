//! `--oferta`: imprime la oferta redactada (lo que hace la CI, sin cuenta ni consola).
//! Si no: conecta con `--consola=<id>` o con la primera encendida de la cuenta, y suena hasta que se lance otra vez (un doble clic
//! suena, otro para, con DELETE a la consola) o `--segundos=N`.
//! Las credenciales las saca auth.rs del almacén de Greenlight; Greenlight tiene que estar cerrado.
//! Sin ventana de consola: el registro va a %LOCALAPPDATA%\XboxSoloAudio\rust.log.

#![cfg_attr(windows, windows_subsystem = "windows")]
#![cfg_attr(not(feature = "webrtc"), deny(dead_code, unused_imports))]

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use xbox_solo_audio::audio::{vigilar_salida, Opus, ReservaCompartida, SALIDA_MAXIMA_US, SALIDA_ULTIMA_US, LLAMADA_NS, OPUS_NS};
#[cfg(feature = "webrtc")]
use xbox_solo_audio::oferta::{generar_oferta, redactar_sdp, Interceptores, INTERCEPTORES};
use xbox_solo_audio::reserva::Reserva;
use xbox_solo_audio::medida::{Fin, Medida};
use xbox_solo_audio::registro::registrar;
use xbox_solo_audio::xhome::{Credenciales, XHome};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};

const TUBERIA: &str = r"\\.\pipe\xbox-solo-audio-rust";

fn argumento(nombre: &str) -> Option<String> {
    std::env::args().find_map(|a| a.strip_prefix(&format!("--{nombre}=")).map(str::to_owned))
}

/// tokio en un solo hilo: con uno por núcleo (16 aquí) la red gastaba el doble
///.
/// `--interceptores=defecto|informes|ninguno` quita interceptores (para medir).
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    #[cfg(feature = "webrtc")]
    if let Some(i) = argumento("interceptores") {
        let _ = INTERCEPTORES.set(Interceptores::desde(&i).ok_or("--interceptores=defecto|informes|ninguno")?);
    }
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let r = runtime.block_on(principal());
    // El registro va por su hilo: que no se pierdan las últimas líneas.
    xbox_solo_audio::registro::vaciar();
    r
}

/// la ruta nativa es la de por defecto (iguala a WebRTC+RED en
/// juego, medido el 02-10); `--webrtc` vuelve a la de antes, si el exe la trae
/// (feature `webrtc`). `--nativo` se sigue aceptando y no cambia nada.
fn es_nativo() -> bool { pedido().nativo }

fn avisar_retiradas(t0: Instant) {
    for r in xbox_solo_audio::modo::retiradas(&std::env::args().skip(1).collect::<Vec<_>>()) { registrar(t0, &format!("opción retirada: --{r}")) }
}

/// La ruta, el relevo, el modo y la reserva: sólo `modo.rs` los decide.
fn pedido() -> xbox_solo_audio::modo::Modo { xbox_solo_audio::modo::modo(&std::env::args().skip(1).collect::<Vec<_>>(), cfg!(feature = "webrtc")) }

/// La sesión de la ruta WebRTC; sin la feature, un error claro.
#[cfg(feature = "webrtc")]
#[allow(clippy::too_many_arguments)]
async fn sesion_webrtc(xhome: XHome, consola: &str, hasta: tokio::time::Instant, prueba_corte: Option<Duration>, t0: Instant, medida: Arc<Medida>, reserva: Option<ReservaCompartida>, parar: Arc<tokio::sync::Notify>, numero: u64) -> Result<Fin, Box<dyn std::error::Error + Send + Sync>> {
    xbox_solo_audio::sesion::sesion(xhome, consola, hasta, prueba_corte, t0, medida, reserva, parar, numero).await
}

#[cfg(not(feature = "webrtc"))]
#[allow(clippy::too_many_arguments)]
async fn sesion_webrtc(_: XHome, _: &str, _: tokio::time::Instant, _: Option<Duration>, _: Instant, _: Arc<Medida>, _: Option<ReservaCompartida>, _: Arc<tokio::sync::Notify>, _: u64) -> Result<Fin, Box<dyn std::error::Error + Send + Sync>> {
    Err("este exe no trae la ruta WebRTC (compilado sin la feature webrtc)".into())
}

/// Las opciones de la ruta WebRTC (sólo con la feature `webrtc`).
#[cfg(feature = "webrtc")]
fn ajustes_webrtc(t0: Instant) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if let Some(s) = argumento("mando-inicial").and_then(|s| s.parse::<u64>().ok()) {
        xbox_solo_audio::sesion::MANDO_INICIAL_S.store(s, Ordering::Relaxed);
        registrar(t0, &format!("DIAGNÓSTICO: mando remoto sólo los primeros {s} s"));
    }
    // RED por defecto; `--sin-red` lo quita (y la oferta vuelve a ser la mínima, sin RED).
    if !std::env::args().any(|a| a == "--sin-red") {
        let copias = argumento("red").and_then(|c| c.parse::<u8>().ok()).unwrap_or(1);
        xbox_solo_audio::oferta::RED_AUDIO.store(true, Ordering::Relaxed);
        xbox_solo_audio::oferta::RED_COPIAS.store(copias, Ordering::Relaxed);
    } else {
        registrar(t0, "DIAGNÓSTICO: sin audio redundante (RED)");
    }
    if std::env::args().any(|a| a == "--nack") {
        xbox_solo_audio::oferta::NACK_AUDIO.store(true, Ordering::Relaxed);
        registrar(t0, "DIAGNÓSTICO: NACK del audio a los 5 ms");
    }
    if let Some(f) = argumento("fisicidad").and_then(|f| f.parse::<u32>().ok()) {
        xbox_solo_audio::canales::FISICIDAD.store(f, Ordering::Relaxed);
        registrar(t0, &format!("DIAGNÓSTICO: physicality {f}"));
    }
    if argumento("systemui").as_deref() == Some("2") {
        xbox_solo_audio::canales::SYSTEMUI_V2.store(true, Ordering::Relaxed);
        registrar(t0, "DIAGNÓSTICO: systemUi versión [0,2,0]");
    }
    if argumento("anuncio").as_deref() == Some("fugaz") {
        xbox_solo_audio::sesion::ANUNCIO_FUGAZ.store(true, Ordering::Relaxed);
        registrar(t0, "DIAGNÓSTICO: anuncio fugaz del mando (add y remove)");
    }
    if let Some(i) = argumento("indice-mando").and_then(|i| i.parse::<u8>().ok()) {
        xbox_solo_audio::canales::INDICE_MANDO.store(i, Ordering::Relaxed);
        registrar(t0, &format!("DIAGNÓSTICO: mando remoto con índice {i}"));
    }
    if let Some(l) = argumento("latido") {
        let n = match l.as_str() { "cliente" => 1, "vacio" => 2, "mensaje" => 3, "puntero" => 4, _ => return Err("--latido=cliente|vacio|mensaje|puntero".into()) };
        xbox_solo_audio::sesion::LATIDO.store(n, Ordering::Relaxed);
        xbox_solo_audio::sesion::SIN_MANDO.store(true, Ordering::Relaxed);
        registrar(t0, &format!("DIAGNÓSTICO: sin mando, latido {l} cada segundo"));
    }
    if std::env::args().any(|a| a == "--sin-mando") {
        xbox_solo_audio::sesion::SIN_MANDO.store(true, Ordering::Relaxed);
        registrar(t0, "DIAGNÓSTICO: sin mando (la consola echará la sesión a los ~58 s)");
    }
    if argumento("interceptores").is_some() {
        registrar(t0, &format!("ajustes: interceptores {:?}", INTERCEPTORES.get().copied().unwrap_or(Interceptores::Defecto)));
    }
    Ok(())
}

async fn principal() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // `--version`: qué exe es y qué rutas trae (la CI lo usa para probar el exe).
    if std::env::args().any(|a| a == "--version") {
        println!("xbox-solo-audio {} (rutas: nativa{}; TLS {})", env!("CARGO_PKG_VERSION"), if cfg!(feature = "webrtc") { " y WebRTC" } else { "" }, if cfg!(feature = "winhttp") { "WinHTTP" } else if cfg!(feature = "rustls") { "rustls" } else { "Schannel" });
        return Ok(());
    }
    if std::env::args().any(|a| a == "--oferta") {
        #[cfg(feature = "webrtc")]
        {
            let oferta = generar_oferta().await?;
            println!("{}", redactar_sdp(&oferta.sdp));
            return Ok(());
        }
        #[cfg(not(feature = "webrtc"))]
        return Err("--oferta es de la ruta WebRTC y este exe no la trae".into());
    }
    // Sin --consola, la primera encendida de la cuenta (auth.rs).
    let consola_pedida = argumento("consola");
    // Sin --segundos, hasta que se pida parar.
    let segundos: u64 = argumento("segundos").and_then(|s| s.parse().ok()).unwrap_or(u64::from(u32::MAX));
    let prueba_corte = argumento("prueba-corte").and_then(|s| s.parse().ok()).map(Duration::from_secs);
    let t0 = Instant::now();
    // la primera línea de cada arranque, para que el MCP `xbox-rust`
    // sepa desde dónde leer (aunque `rust.log` rote o el MCP se reinicie).
    registrar(t0, &format!("arranque: xbox-solo-audio {} (rutas: nativa{}) {}", env!("CARGO_PKG_VERSION"), if cfg!(feature = "webrtc") { " y WebRTC" } else { "" },
        std::env::args().skip(1).collect::<Vec<_>>().join(" ")));
    // lo que sale por la tarjeta, a un WAV (con la muestra de cada relevo en el registro).
    if let Some(ruta) = argumento("volcar-wav") {
        match xbox_solo_audio::volcado::abrir(&ruta) {
            Ok(()) => registrar(t0, &format!("volcado: la salida va también a {ruta} (48 kHz estéreo s16)")),
            Err(e) => registrar(t0, &format!("volcado: no se pudo crear {ruta} ({e})")),
        }
    }
    // La parada del programa: la piden el segundo clic (tubería) y `parar` por depuración.
    let parar = Arc::new(tokio::sync::Notify::new());
    if let Some(puerto) = argumento("depurar").and_then(|p| p.parse::<u16>().ok()) {
        xbox_solo_audio::depurar::escuchar(puerto, t0, parar.clone()).await?;
    }
    for ajuste in std::env::args().filter_map(|a| a.strip_prefix("--ajuste-play=").map(str::to_owned)) {
        let Some((clave, valor)) = ajuste.split_once('=') else { return Err("--ajuste-play=clave=valor".into()) };
        registrar(t0, &format!("DIAGNÓSTICO: /play settings.{clave} = {valor}"));
        if let Ok(mut a) = xbox_solo_audio::xhome::AJUSTES_PLAY.lock() { a.push((clave.to_owned(), valor.to_owned())); }
    }
    // Las opciones de WebRTC sólo cuentan con `--webrtc`: en la
    // nativa, `--sin-mando` es `--sin-entrada` y RED/NACK no existen.
    #[cfg(feature = "webrtc")]
    if !es_nativo() { ajustes_webrtc(t0)? }
    if !es_nativo() { registrar(t0, "RUTA WebRTC (--webrtc)") }
    // `--nativo`: la ruta Nano V3 nativa (src/nano), sin WebRTC. Sus opciones:
    // `--sin-entrada`, `--entrada-mando`, `--entrada-vacia`, `--entrada-hasta=S`, `--entrada-cada=MS`, `--chat-silencio`, `--micro[=nombre]`, `--party-activa`, `--chat-pt=N`, `--espera-mtu=MS`
    // y `--video=FPS,ANCHO,ALTO`; el relevo y la reserva, en `modo.rs`. Los experimentos cerrados, fuera.
    avisar_retiradas(t0);
    let pedido = pedido();
    let nativo = pedido.nativo.then(|| {
        use xbox_solo_audio::nano::protocolo::Entrada;
        let mut a = xbox_solo_audio::nano::sesion::Ajustes::default();
        if pedido.sin_juego { a.protocolo.entrada = Entrada::Ninguna }
        if std::env::args().any(|a| a == "--entrada-mando") { a.protocolo.entrada = Entrada::Mando }
        if std::env::args().any(|a| a == "--entrada-vacia") { a.protocolo.entrada = Entrada::Vacia }
        a.chat_silencio = std::env::args().any(|a| a == "--chat-silencio");
        // `--micro` (el predeterminado) o `--micro=<parte del nombre>`;
        // `--party-activa` y `--chat-pt=N`, para probar qué hace falta en la consola.
        let micro = argumento("micro").or_else(|| std::env::args().any(|a| a == "--micro").then(String::new));
        if let Some(nombre) = micro {
            match xbox_solo_audio::micro::abrir(&nombre, t0) {
                Ok(m) => a.micro = Some(m),
                Err(e) => registrar(t0, &format!("micro: no se pudo abrir ({e}); sigue sin micro")),
            }
        }
        a.protocolo.party_activa = std::env::args().any(|a| a == "--party-activa");
        if let Some(pt) = argumento("chat-pt").and_then(|s| s.parse().ok()) { a.protocolo.chat_pt = pt }
        if a.micro.is_some() || a.protocolo.party_activa { registrar(t0, &format!("ChatAudio: micro {}, setPartyChatActive {}, tipo RTP {}", a.micro.is_some(), a.protocolo.party_activa, a.protocolo.chat_pt)) }
        if let Some(ms) = argumento("espera-mtu").and_then(|v| v.parse().ok()) { a.protocolo.espera_mtu_ms = ms; registrar(t0, &format!("DIAGNÓSTICO: hasta {ms} ms más esperando el acuse de nuestra sonda de MTU")) }
        a.protocolo.entrada_hasta_us = argumento("entrada-hasta").and_then(|s| s.parse::<u64>().ok()).map(|s| s * 1_000_000);
        // en modo juego, una trama de entrada por segundo (como el mando en
        // reposo de WebRTC: cualquier trama reinicia el vigilante); a 12 ms eran 80
        // veces más subida y trabajo para la consola. `--entrada-cada=MS` lo cambia.
        a.entrada_cada = Duration::from_millis(argumento("entrada-cada").and_then(|s| s.parse::<u64>().ok()).unwrap_or(1000).max(1));
        if let Some(v) = argumento("video") {
            let n: Vec<u32> = v.split(',').filter_map(|x| x.parse().ok()).collect();
            if let [fps, ancho, alto] = n[..] { a.protocolo.video = (fps, ancho, alto) }
        }
        // en modo música (sin entrada), por defecto,
        // arranque en paralelo con sondeos rápidos y el MTU acusado en el acto.
        // `--arranque-serie` y `--mtu-espera` vuelven a lo de antes; fuera del
        // modo música, `--arranque-paralelo` y `--mtu-en-el-acto` lo activan.
        let musica = a.protocolo.entrada == Entrada::Ninguna;
        let hay = |x: &str| std::env::args().any(|a| a == x);
        // también en modo juego (lo que tarda en sonar el clic).
        a.arranque_paralelo = !hay("--arranque-serie");
        a.protocolo.mtu_en_el_acto = !hay("--mtu-espera");
        if a.arranque_paralelo { xbox_solo_audio::xhome::RAPIDO.store(true, Ordering::Relaxed) }
        // en modo música, por defecto a 500 ms del /play (el xccs tarda
        // ~0,9-1,6 s y el audio llega a ~1,5-1,9 s); a 0 adelantaba a Spotify y se
        // perdía música. `--sin-play-anticipado` lo quita.
        a.play_anticipado = argumento("play-anticipado").and_then(|v| v.parse().ok())
            .or_else(|| musica.then_some(500)).filter(|_| !hay("--sin-play-anticipado"));
        registrar(t0, &format!("arranque {}, MTU {}{}", if a.arranque_paralelo { "en paralelo (sondeos a 50/25 ms)" } else { "en serie" },
            if a.protocolo.mtu_en_el_acto { "acusado en el acto" } else { "tras nuestras 16 sondas" },
            a.play_anticipado.map(|ms| format!(", Play anticipado a {ms} ms del /play de cada relevo")).unwrap_or_default()));
        registrar(t0, &format!("RUTA NATIVA (Nano V3, sin WebRTC): entrada {:?} cada {} ms{}, vídeo {:?}, silencio por chat {}",
            a.protocolo.entrada, a.entrada_cada.as_millis(),
            a.protocolo.entrada_hasta_us.map(|h| format!(" hasta el segundo {}", h / 1_000_000)).unwrap_or_default(),
            a.protocolo.video, a.chat_silencio));
        a
    });
    // `--reenganchar`: al cortarse no se hace DELETE y el /play
    // siguiente vuelve a la misma sesión; `--reenganchar=id` la pide además por
    // su id (`clientSessionId`). Cada intento registra «primer audio a N ms del /play».
    if let Some(a) = std::env::args().find(|a| a == "--reenganchar" || a.starts_with("--reenganchar=")) {
        let modo = if a.ends_with("=id") { 2 } else { 1 };
        xbox_solo_audio::xhome::REENGANCHAR.store(modo, Ordering::Relaxed);
        registrar(t0, &format!("DIAGNÓSTICO: re-enganche{}", if modo == 2 { " pidiendo la sesión por su id" } else { "" }));
    }
    // `--reserva-inicial=MS,S`: reserva de MS ms los primeros S s de
    // cada sesión y luego la normal (las pérdidas se concentran al arrancar).
    // Por la ruta nativa (sin RED) va por defecto 40 ms los primeros 20 s (A12:
    // 1+0+0 huecos frente a 0+8+1 sin ella); `--reserva-inicial=0,0` la quita.
    let reserva_inicial = pedido.reserva_inicial;
    let generacion = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let hasta = tokio::time::Instant::now() + Duration::from_secs(segundos);

    // Instancia única por tubería con nombre: si ya hay una sonando, se le pide
    // que pare (ella hace el DELETE) y esta sale. Es el «otro clic para».
    // `--sin-tuberia` (diagnóstico): sin instancia única, para probar dos
    // sesiones solapadas contra la misma consola.
    let tuberia = if std::env::args().any(|a| a == "--sin-tuberia") { Err(std::io::Error::other("sin tubería")) } else { ServerOptions::new().first_pipe_instance(true).create(TUBERIA) };
    if tuberia.is_err() && std::env::args().any(|a| a == "--sin-tuberia") {
        registrar(t0, "DIAGNÓSTICO: sin instancia única");
    } else { match tuberia {
        Err(_) => {
            if let Ok(mut otra) = ClientOptions::new().open(TUBERIA) { let _ = otra.write_all(b"parar\n").await; }
            return Ok(());
        }
        Ok(mut servidor) => {
            let parar = parar.clone();
            tokio::spawn(async move {
                loop {
                    if servidor.connect().await.is_ok() {
                        let mut leido = [0u8; 16];
                        let n = servidor.read(&mut leido).await.unwrap_or(0);
                        if leido[..n].starts_with(b"parar") {
                            registrar(t0, "segundo lanzamiento: parada pedida");
                            parar.notify_one();
                        }
                    }
                    servidor = match ServerOptions::new().create(TUBERIA) { Ok(s) => s, Err(_) => break };
                }
            });
        }
    } }


    // Audio: reserva de 20 ms y salida
    // compartida. --sin-sonido sólo mide. El flujo se mantiene vivo hasta el final.
    // `--reserva=N` (diagnóstico): reserva mínima en ms (por defecto 20, un paquete).
    // Modo música: sin entrada de juego la latencia da igual y una
    // reserva grande y fija suena mejor que una ajustada al mínimo. Por
    // defecto con `--relevo` y `--sin-entrada`/`--sin-mando`, o con
    // `--reserva-musica=MS` (150 por defecto). El modo juego sigue en 20 ms.
    let (tapar, reserva_ms) = (pedido.tapar, pedido.reserva_ms);
    let musica = pedido.reserva_fija.then_some(reserva_ms);
    if let Some(m) = musica { registrar(t0, &format!("MODO MÚSICA: reserva fija de {m} ms")); }
    else if reserva_ms != 20 { registrar(t0, &format!("DIAGNÓSTICO: reserva de {reserva_ms} ms")); }
    let reserva: Option<ReservaCompartida> = if std::env::args().any(|a| a == "--sin-sonido") { None } else {
        let ahora = t0.elapsed().as_secs_f64() * 1000.0;
        let mut r = Reserva::nueva(Opus::nuevo()?, reserva_ms, reserva_ms, reserva_ms.max(200), 60_000.0, ahora);
        if tapar {
            r.tapar = Some(Default::default());
            xbox_solo_audio::audio::TAPAR.store(true, Ordering::Relaxed);
            registrar(t0, &format!("TAPAR EL RELEVO: reserva de {reserva_ms} ms, silencio del relevo descartado, empalme por correlación y estiramiento ≤2,5 %"));
        }
        let r = Arc::new(std::sync::Mutex::new(r));
        if tapar { xbox_solo_audio::audio::registrar_tapar(r.clone(), t0) }
        if pedido.play_en_pausa {
            xbox_solo_audio::relevo::reaccionar_a_la_pausa(r.clone(), t0);
            registrar(t0, &format!("pausa: Play por la nube en cuanto lo nuevo cae en la pausa de Spotify y cada {} ms mientras siga ({} como mucho)", xbox_solo_audio::relevo::REACCION_CADA_MS, xbox_solo_audio::relevo::REACCION_TOPE));
        }
        Some(r)
    };
    // `--salida-iac3` (A9): IAudioClient3 con el periodo mínimo en vez de cpal.
    if std::env::args().any(|a| a == "--salida-iac3") {
        xbox_solo_audio::audio::SALIDA_IAC3.store(true, Ordering::Relaxed);
        registrar(t0, "DIAGNÓSTICO: salida por IAudioClient3 (periodo mínimo)");
    }
    if let Some(r) = &reserva { vigilar_salida(argumento("salida").unwrap_or_default(), r.clone(), t0) }
    // `--raton-teclado` (A18): el ratón y el teclado del PC como mando (F8 los captura y los suelta).
    if std::env::args().any(|a| a == "--raton-teclado") {
        xbox_solo_audio::raton_teclado::ACTIVADO.store(true, Ordering::Relaxed);
        xbox_solo_audio::raton_teclado::capturar();
        registrar(t0, "ratón y teclado: listos; F8 los captura para la consola y los suelta (WASD, ratón, clics, Espacio A, C B, R X, E Y, Q/F bumpers)");
    }
    // `--volumen=N`: ganancia de la salida en %, con limitador por encima de 100.
    if let Some(v) = argumento("volumen").and_then(|v| v.trim_end_matches('%').parse::<f32>().ok()) {
        xbox_solo_audio::audio::fijar_volumen(v);
        registrar(t0, &format!("volumen: {v} % (limitador suave por encima de 100)"));
    }

    let medida = Arc::new(Medida::default());
    // `--vigilar-volumen`: cada segundo, paquetes, ocultados y el nivel
    // RMS del audio descodificado. ¿Atenúa la consola el audio antes de un Kick?
    if std::env::args().any(|a| a == "--vigilar-volumen") {
        xbox_solo_audio::audio::VIGILAR_VOLUMEN.store(true, Ordering::Relaxed);
        let (m, r) = (medida.clone(), reserva.clone());
        tokio::spawn(async move {
            use xbox_solo_audio::audio::{VOLUMEN_MUESTRAS, VOLUMEN_SUMA};
            let mut cada = xbox_solo_audio::red::intervalo(Duration::from_secs(1));
            let (mut p0, mut o0) = (0u64, 0u64);
            loop {
                cada.tick().await;
                let p = m.paquetes.load(Ordering::Relaxed);
                let o = r.as_ref().and_then(|r| r.lock().ok().map(|r| r.cuenta.ocultados)).unwrap_or(0);
                let (suma, n) = (VOLUMEN_SUMA.swap(0, Ordering::Relaxed), VOLUMEN_MUESTRAS.swap(0, Ordering::Relaxed));
                let db = if n == 0 { "sin audio".to_owned() } else {
                    let rms = (suma as f64 / n as f64).sqrt() / 32768.0;
                    if rms > 0.0 { format!("{:.1} dBFS", 20.0 * rms.log10()) } else { "silencio digital".into() }
                };
                registrar(t0, &format!("volumen: {} paq/s, {} ocultados, {db}", p - p0, o - o0));
                (p0, o0) = (p, o);
            }
        });
    }
    let m = medida.clone();
    let r = reserva.clone();
    tokio::spawn(async move {
        let mut cada = xbox_solo_audio::red::intervalo(Duration::from_secs(10));
        cada.tick().await;
        let (mut paquetes, mut bytes) = (0, 0);
        loop {
            cada.tick().await;
            let (p, b) = (m.paquetes.load(Ordering::Relaxed), m.bytes.load(Ordering::Relaxed));
            let estado = r.as_ref().and_then(|r| r.lock().ok().map(|r| format!(", reserva {} ms (objetivo {}), {:?}", r.bloques_pendientes() * 10, r.objetivo_ms(), r.cuenta))).unwrap_or_default();
            let salida = if r.is_some() { format!(", salida {:.1} ms (máx. {:.1})", SALIDA_ULTIMA_US.load(Ordering::Relaxed) as f64 / 1000.0, SALIDA_MAXIMA_US.swap(0, Ordering::Relaxed) as f64 / 1000.0) } else { String::new() };
            // % de un núcleo en 10 s: ns / 1e10 * 100.
            let cpu = if r.is_some() { format!(", CPU audio {:.2} % (opus {:.2} %)", LLAMADA_NS.swap(0, Ordering::Relaxed) as f64 / 1e8, OPUS_NS.swap(0, Ordering::Relaxed) as f64 / 1e8) } else { String::new() };
            // la memoria y lo que la podría llenar.
            let memoria = xbox_solo_audio::medida::memoria().map(|(p, t)| format!("privada {:.1} MB, de trabajo {:.1} MB", p as f64 / 1e6, t as f64 / 1e6)).unwrap_or_else(|| "sin datos".into());
            let tamanos = r.as_ref().and_then(|r| r.lock().ok().map(|r| r.tamanos())).unwrap_or_default();
            let c = &xbox_solo_audio::elastico::CIFRAS;
            registrar(t0, &format!("memoria: {memoria}; reserva: {tamanos}; elástico: entrada {} y salida {} muestras; registro: {} líneas en cola",
                c.entrada.load(Ordering::Relaxed), c.salida.load(Ordering::Relaxed), xbox_solo_audio::registro::en_cola()));
            registrar(t0, &format!("audio: {} paq/s, {:.1} kbps, CELT FB estéreo {} de {}, otras fuentes {}, redundantes {}{estado}{salida}{cpu}",
                (p - paquetes) as f64 / 10.0, (b - bytes) as f64 * 8.0 / 10_000.0,
                m.celt_fb_estereo.load(Ordering::Relaxed), p, m.otros.load(Ordering::Relaxed), m.redundantes.load(Ordering::Relaxed)));
            (paquetes, bytes) = (p, b);
        }
    });
    // Reintentos (reintento.rs): 1, 2, 4, 8 y 16 s y luego, sin rendirse, cada 30 s
    // si la consola está encendida; un intento que dio audio vuelve a contar desde cero. Cada intento pide
    // credenciales nuevas (el token de /connect es de corta vida).
    // `--relevo=N`: una sesión nueva cada N s contados desde
    // el /play de la actual, en paralelo y sin parar la vieja (la consola la
    // echa con KickByNewSession). Por la ruta nativa o por WebRTC (con
    // `--sin-mando` no hay modo remoto). El vigilante de la consola cuenta ~52 s
    // desde el /play y, tras un Kick, el Provisioning siguiente tarda ~28 s:
    // hay que relevar antes (40 s). Las credenciales de la siguiente se piden
    // 10 s antes, para que su /play salga en el instante N. Una sola reserva y
    // una sola salida para todas.
    // `--relevo-reenganche=N`: el mismo relevo, pero sin DELETE de
    // la vieja ni cierre de la «abandonada», para que el /play nuevo pueda
    // devolver LA MISMA sesión (re-enganche: Provisioned en ~50 ms, medido) en
    // vez de una nueva. El registro dice si el id coincide y la huella de la clave.
    // En modo música por la ruta nativa (`--nativo --sin-entrada --relevo=N`) el
    // relevo es por re-enganche (hueco de 1,4-2,6 s frente a 3,2-4,6).
    // en modo música, `--relevo-provisioned=S` solo ya activa el relevo
    // (es lo que manda; N s desde el /play sólo cuenta mientras no se sabe Provisioned).
    let reenganche = pedido.relevo.as_ref().filter(|r| r.reenganche).map(|r| r.cada_s);
    // Play por la nube tras cada relevo sin entrada (`--play-tras-relevo=0,1000`
    // ms desde que la sesión nueva da audio; `--sin-play-tras-relevo` lo quita).
    let play_tras_relevo = pedido.play_tras_relevo.clone();
    if let Some(p) = &play_tras_relevo { registrar(t0, &format!("relevo: Media/Play por la nube tras cada relevo, a {p:?} ms")) }
    // y al empezar, cuando la primera sesión da audio (`--play-al-empezar=0,3000`
    // ms; `--sin-play-al-empezar` lo quita): si Spotify estaba en pausa, suena ya.
    let play_al_empezar = pedido.play_al_empezar.clone();
    if let Some(p) = &play_al_empezar {
        registrar(t0, &format!("Media/Play por la nube al empezar, a {p:?} ms del primer audio"));
        if let Ok(mut e) = xbox_solo_audio::relevo::PLAY_AL_EMPEZAR.lock() { *e = Some(p.clone()); }
    }
    if reenganche.is_some() {
        xbox_solo_audio::xhome::REENGANCHAR.store(1, Ordering::Relaxed);
        registrar(t0, "RELEVO POR RE-ENGANCHE: sin DELETE entre sesiones");
    }
    // en modo música por re-enganche, por defecto a 57 s de Provisioned (modo.rs)
    // (`--relevo-provisioned=S`; 0 vuelve a contar N s desde el /play).
    let relevo_provisioned = pedido.relevo.as_ref().and_then(|r| r.provisioned_s);
    if let Some(s) = relevo_provisioned { registrar(t0, &format!("relevo: la sesión siguiente a {s} s de que la actual llegue a Provisioned (y nunca antes de que dé audio)")) }
    for aviso in &pedido.avisos { registrar(t0, aviso) }
    if let Some(n) = pedido.relevo.as_ref().map(|r| r.cada_s) {
        registrar(t0, &format!("RELEVO ({}): una sesión nueva cada {n} s desde el /play de la anterior", if nativo.is_some() { "nativa" } else { "WebRTC" }));
        let mut vivas: Vec<(Arc<tokio::sync::Notify>, tokio::task::JoinHandle<()>)> = Vec::new();
        let mut numero = 0u64;
        let mut preparadas: Option<xbox_solo_audio::xhome::Credenciales> = None;
        // sesiones seguidas que terminan sin dar audio (consola
        // apagada, red caída): la misma espera que fuera del relevo, en vez de
        // pedir credenciales y /play sin parar.
        let (mut fallos, mut para_mirar): (u32, Option<(String, Option<String>)>) = (0, None);
        'relevo: loop {
            if fallos > 0 && !esperar_reintento(fallos, "la sesión anterior no dio audio o no hubo credenciales", &parar, hasta, &para_mirar, t0).await { break 'relevo }
            numero += 1;
            let cred = match preparadas.take() {
                Some(c) => c,
                None => match credenciales_aparte(consola_pedida.is_none()).await {
                    Ok(c) => c,
                    // la misma espera que tras un fallo (red caída: no
                    // pedir a Microsoft cada 2 s para siempre).
                    Err(e) => { registrar(t0, &format!("relevo: sin credenciales ({e})")); fallos += 1; numero -= 1; continue }
                },
            };
            let Some(consola) = consola_pedida.clone().or_else(|| cred.consola.clone()) else { return Err("no encuentro ninguna consola en la cuenta".into()) };
            if let Some(x) = &cred.xccs { para_mirar = Some((x.clone(), Some(consola.clone()))) }
            // Spotify abierto al empezar la música (una vez, con las primeras credenciales).
            if let (1, Some(app), Some(x)) = (numero, &pedido.app_al_empezar, &cred.xccs) {
                xbox_solo_audio::relevo::abrir_app_al_empezar(x.clone(), consola.clone(), app.clone(), t0);
            }
            // Play por la nube tras cada relevo sin entrada, con el token de esta vuelta (siempre fresco).
            if let (true, Some(x)) = (play_tras_relevo.is_some() || play_al_empezar.is_some(), &cred.xccs) {
                let esperas = play_tras_relevo.clone().unwrap_or_default();
                if let Ok(mut p) = xbox_solo_audio::relevo::PLAY_TRAS_RELEVO.lock() { *p = Some((x.clone(), consola.clone(), esperas)); }
                // la conexión con xccs abierta antes de que el Play haga falta.
                let etiqueta = xbox_solo_audio::relevo::etiqueta(numero);
                tokio::spawn(async move {
                    match xbox_solo_audio::auth::calentar_xccs().await {
                        Ok((a, b)) => registrar(t0, &format!("{etiqueta}xccs: dos conexiones calientes ({a} y {b} ms)")),
                        Err(e) => registrar(t0, &format!("{etiqueta}xccs: no se pudo calentar la conexión ({e})")),
                    }
                });
            }
            let xhome = XHome::nuevo(cred)?;
            let parar_esta = Arc::new(tokio::sync::Notify::new());
            let (m, r, p) = (medida.clone(), reserva.clone(), parar_esta.clone());
            let ajustes = nativo.clone().map(|mut a| { a.numero = numero; a });
            let inicio = tokio::time::Instant::now();
            let mut tarea = tokio::spawn(xbox_solo_audio::relevo::ETIQUETA.scope(xbox_solo_audio::relevo::etiqueta(numero), async move {
                let fin = match &ajustes {
                    Some(a) => xbox_solo_audio::nano::sesion::sesion(xhome, &consola, hasta, t0, m, r, p, a).await,
                    None => sesion_webrtc(xhome, &consola, hasta, None, t0, m, r, p, numero).await,
                };
                match fin {
                    Ok(f) => xbox_solo_audio::relevo::reg(t0, &format!("fin: {f:?}")),
                    Err(e) => xbox_solo_audio::relevo::reg(t0, &format!("error: {e}")),
                }
            }));
            vivas.retain(|(_, h)| !h.is_finished());
            // Dos esperas: hasta preparar las credenciales de la siguiente y hasta su /play.
            // Si la actual termina antes, la siguiente sale ya. Con
            // `--relevo-provisioned=S`, el /play de la siguiente va a S s de que
            // ésta llegue a Provisioned (el vigilante cuenta 60 s desde
            // ahí); mientras no se sabe, N s desde su /play.
            // nunca se releva a una sesión que aún no ha dado audio (con la
            // red lenta, contar N s desde su /play la mataba antes de sonar).
            let objetivo = |preparar: bool| {
                if !xbox_solo_audio::relevo::dio_audio(numero) { return tokio::time::Instant::now() + Duration::from_secs(3600) }
                let base = match relevo_provisioned.zip(xbox_solo_audio::relevo::hitos(numero).provisioned) {
                    Some((s, p)) => tokio::time::Instant::from_std(p) + Duration::from_secs(s),
                    None => inicio + Duration::from_secs(n),
                };
                if preparar { base.checked_sub(Duration::from_secs(10)).unwrap_or(base) } else { base }
            };
            let mut terminada = false;
            for preparar in [true, false] {
                while !terminada {
                    let plazo = objetivo(preparar);
                    if tokio::time::Instant::now() >= plazo { break }
                    tokio::select! {
                        _ = tokio::time::sleep_until(plazo.min(tokio::time::Instant::now() + Duration::from_millis(500))) => {}
                        _ = &mut tarea => { terminada = true }
                        _ = parar.notified() => { vivas.push((parar_esta, tarea)); break 'relevo }
                        _ = tokio::time::sleep_until(hasta) => { vivas.push((parar_esta, tarea)); break 'relevo }
                    }
                }
                // Las credenciales de la siguiente, 10 s antes; si fallan, otra vez
                // cada 2 s hasta conseguirlas o hasta su /play.
                while preparar && !terminada && preparadas.is_none() {
                    match credenciales_aparte(consola_pedida.is_none()).await {
                        Ok(c) => preparadas = Some(c),
                        Err(e) => {
                            registrar(t0, &format!("relevo: no se pudieron preparar las credenciales ({e}); reintento en 2 s"));
                            if tokio::time::Instant::now() + Duration::from_secs(2) >= objetivo(false) { break }
                            tokio::time::sleep(Duration::from_secs(2)).await;
                        }
                    }
                }
            }
            if !terminada { vivas.push((parar_esta, tarea)) }
            if xbox_solo_audio::relevo::dio_audio(numero) { fallos = 0 } else if terminada { fallos += 1 }
        }
        // Parar todas y esperar su cierre. Una que esté a medio preparar sigue
        // hasta poder cerrarse (Provisioning tarda hasta ~30 s tras un Kick):
        // salir antes la dejaría abierta y la consola en «remoto».
        xbox_solo_audio::relevo::PARADA.store(true, Ordering::Relaxed);
        for (p, _) in &vivas { p.notify_one() }
        for (_, h) in vivas { let _ = tokio::time::timeout(Duration::from_secs(90), h).await; }
        registrar(t0, "parada");
        return Ok(());
    }
    let mut reintentos = 0u32;
    // la autorización de xccs y la consola del último intento, para
    // mirar si está encendida mientras se espera (sin pedir credenciales cada vez).
    let mut para_mirar: Option<(String, Option<String>)> = None;
    loop {
        let paquetes_antes = medida.paquetes.load(Ordering::Relaxed);
        if let (Some((ms, s)), Some(r)) = (reserva_inicial, &reserva) {
            if let Ok(mut r) = r.lock() { r.ajustar(ms, ms); }
            registrar(t0, &format!("DIAGNÓSTICO: reserva de {ms} ms los primeros {s} s"));
            let (r, g) = (r.clone(), generacion.clone());
            let esta = g.fetch_add(1, Ordering::Relaxed) + 1;
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(s)).await;
                // Sólo si sigue siendo la misma sesión.
                if g.load(Ordering::Relaxed) == esta {
                    if let Ok(mut r) = r.lock() { r.ajustar(reserva_ms, reserva_ms); }
                    registrar(t0, &format!("reserva: vuelve a {reserva_ms} ms"));
                }
            });
        }
        let resultado: Result<Fin, Box<dyn std::error::Error + Send + Sync>> = async {
            let credenciales = xbox_solo_audio::auth::autenticar(consola_pedida.is_none()).await?;
            let consola = consola_pedida.clone().or_else(|| credenciales.consola.clone()).ok_or("no encuentro ninguna consola en la cuenta")?;
            if let Some(x) = &credenciales.xccs { para_mirar = Some((x.clone(), Some(consola.clone()))) }
            registrar(t0, &format!("autenticado; región {}", credenciales.region));
            if credenciales.encendida { registrar(t0, "la consola estaba en reposo: encendida a distancia") }
            // La prueba de corte, sólo en el primer intento.
            if let Some(ajustes) = &nativo {
                return xbox_solo_audio::nano::sesion::sesion(XHome::nuevo(credenciales)?, &consola, hasta, t0, medida.clone(), reserva.clone(), parar.clone(), ajustes).await;
            }
            sesion_webrtc(XHome::nuevo(credenciales)?, &consola, hasta, if reintentos == 0 { prueba_corte } else { None }, t0, medida.clone(), reserva.clone(), parar.clone(), 0).await
        }.await;
        let motivo = match resultado {
            Ok(Fin::Cumplido) => break,
            Ok(Fin::Corte(motivo)) => motivo,
            Err(e) => e.to_string(),
        };
        // se vuelve a empezar la cuenta sólo si
        // este intento dio audio (antes, si duró más de 60 s: `autenticar` tarda
        // eso en rendirse con una consola que no enciende, y nunca se esperaba lento).
        if medida.paquetes.load(Ordering::Relaxed) > paquetes_antes { reintentos = 0 }
        reintentos += 1;
        if !esperar_reintento(reintentos, &motivo, &parar, hasta, &para_mirar, t0).await { break }
    }
    xbox_solo_audio::relevo::PARADA.store(true, Ordering::Relaxed);
    registrar(t0, "parada");
    Ok(())
}

/// la espera antes del intento `reintentos` (1 el primero tras un
/// fallo): 1-2-4-8-16 s y luego, sin rendirse, mirar cada 30 s si la consola
/// está encendida (con la autorización de xccs del último intento). `false`:
/// piden parar o se cumplió `--segundos`.
async fn esperar_reintento(reintentos: u32, motivo: &str, parar: &tokio::sync::Notify, hasta: tokio::time::Instant, para_mirar: &Option<(String, Option<String>)>, t0: Instant) -> bool {
    use xbox_solo_audio::reintento::{espera, Espera, LENTO};
    let seguir = || tokio::time::Instant::now() < hasta;
    match espera(reintentos) {
        Espera::Rapida(d) => {
            registrar(t0, &format!("reconectando en {} s ({motivo})", d.as_secs()));
            // La espera se corta si piden parar o se cumple `--segundos`.
            tokio::time::timeout_at((tokio::time::Instant::now() + d).min(hasta), parar.notified()).await.is_err() && seguir()
        }
        Espera::Lenta => {
            registrar(t0, &format!("esperando a la consola ({motivo}): se mira cada {} s si está encendida y vuelve a sonar sola", LENTO.as_secs()));
            loop {
                if tokio::time::timeout_at((tokio::time::Instant::now() + LENTO).min(hasta), parar.notified()).await.is_ok() || !seguir() { return false }
                let Some((x, c)) = para_mirar else { return true };
                match xbox_solo_audio::auth::consola_encendida(x, c.as_deref()).await {
                    Ok(true) => { registrar(t0, "esperando a la consola: encendida; se vuelve a conectar"); return true }
                    Ok(false) => {}
                    // Red caída o token caducado: se intenta entero (credenciales nuevas).
                    Err(e) => { registrar(t0, &format!("esperando a la consola: no se pudo mirar ({e}); se intenta igual")); return true }
                }
            }
        }
    }
}

/// `credenciales()` en un hilo aparte con su propio runtime. La autenticación
/// hace trabajo bloqueante (`tasklist`, cerrojo con esperas, ficheros): en el
/// hilo único de tokio dejaba muda más de 1 s a la sesión nativa viva del
/// relevo, y la consola la cierra al segundo sin respuesta (Disconnect a los
/// N−10 s, medido el 01-10). Las demás sesiones siguen atendidas mientras tanto.
async fn credenciales_aparte(elegir_consola: bool) -> Result<Credenciales, Box<dyn std::error::Error + Send + Sync>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let r = tokio::runtime::Builder::new_current_thread().enable_all().build()
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
            .and_then(|rt| rt.block_on(xbox_solo_audio::auth::autenticar(elegir_consola)));
        let _ = tx.send(r);
    });
    rx.await.map_err(|_| "el hilo de credenciales terminó sin respuesta")?
}

