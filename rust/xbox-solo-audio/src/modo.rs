//! ¿qué ruta, hay relevo, en qué modo, qué reserva? Se decidía en
//! cinco sitios de `main.rs` con condiciones distintas, y el acceso de música
//! con `--relevo-provisioned` activaba el re-enganche pero no la reserva de
//! música (cortes en cada relevo, sin que se viera hasta oírlo). Ahora sólo
//! aquí, con una prueba por cada forma de pedirlo; `tools/guardia.mjs` falla si
//! alguien vuelve a mirar estas opciones fuera de este fichero.

/// El relevo de music por defecto: a 57 s de Provisioned (el vigilante echa a
/// los 59,8; 10 + 10 relevos a 57 sin una expulsión, medido el 02-10).
pub const PROVISIONED_MUSICA_S: u64 = 57;

/// Spotify en la Tienda de Xbox (`oneStoreProductId`).
pub const SPOTIFY: &str = "9NFQ49H668TB";

#[derive(Debug, Clone, PartialEq)]
pub struct Relevo {
    /// N s desde el `/play` de la sesión viva: lo que cuenta mientras no se
    /// sabe su Provisioned (o siempre, sin `provisioned_s`).
    pub cada_s: u64,
    /// S s desde el Provisioned de la sesión viva.
    pub provisioned_s: Option<u64>,
    /// Sin DELETE entre sesiones: el `/play` nuevo vuelve a la misma (modo música).
    pub reenganche: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Modo {
    /// La ruta nativa (la de por defecto); `--webrtc` la otra, si el exe la trae.
    pub nativo: bool,
    /// Sin entrada de juego (`--sin-entrada` o `--sin-mando`): la consola no
    /// entra en modo juego remoto y Spotify sigue sonando.
    pub sin_juego: bool,
    pub relevo: Option<Relevo>,
    /// Modo música: sin entrada y con relevo.
    pub musica: bool,
    /// La reserva (ms) y si tapa el hueco del relevo (`--tapar-relevo`).
    pub reserva_ms: u32,
    pub tapar: bool,
    /// Reserva fija de música (tapando, `--reserva-musica` o el modo música).
    pub reserva_fija: bool,
    /// Reserva más grande los primeros S s (ms, S); fuera del modo música.
    pub reserva_inicial: Option<(u32, u64)>,
    /// Lo pedido que no hace nada, para el registro.
    pub avisos: Vec<String>,
    /// `Media/Play` por la nube tras cada relevo y al empezar (ms del primer audio).
    pub play_tras_relevo: Option<Vec<u64>>,
    pub play_al_empezar: Option<Vec<u64>>,
    /// Play en cuanto la reserva ve la pausa de Spotify tras un
    /// relevo (con tapado y Play tras relevo; `--sin-play-en-pausa` lo quita).
    pub play_en_pausa: bool,
    /// en modo música, la app (producto de la Tienda) que se abre al
    /// empezar si en la consola no suena nada (al acabar una sesión remota la
    /// consola cierra Spotify). `--app=<id>` la cambia; `--sin-app`, nada.
    pub app_al_empezar: Option<String>,
}

fn valor<'a>(args: &'a [String], nombre: &str) -> Option<&'a str> {
    let prefijo = format!("--{nombre}=");
    args.iter().find_map(|a| a.strip_prefix(&prefijo))
}

fn hay(args: &[String], opcion: &str) -> bool { args.iter().any(|a| a == opcion) }

fn numero<T: std::str::FromStr>(args: &[String], nombre: &str) -> Option<T> { valor(args, nombre).and_then(|v| v.parse().ok()) }

fn lista(args: &[String], nombre: &str, defecto: &[u64]) -> Vec<u64> {
    valor(args, nombre).map(|v| v.split(',').filter_map(|x| x.parse().ok()).collect()).unwrap_or_else(|| defecto.to_vec())
}

/// Decide el modo a partir de los argumentos (sin el nombre del programa).
/// `con_webrtc`: si el exe trae la ruta WebRTC (feature `webrtc`).
pub fn modo(args: &[String], con_webrtc: bool) -> Modo {
    let nativo = !con_webrtc || !hay(args, "--webrtc");
    let sin_juego = hay(args, "--sin-entrada") || hay(args, "--sin-mando");
    // `--relevo-provisioned` sin número: el de por defecto; `=0`: contar desde el /play.
    let provisioned_pedido: Option<u64> = numero(args, "relevo-provisioned")
        .or_else(|| hay(args, "--relevo-provisioned").then_some(PROVISIONED_MUSICA_S));
    let relevo = if let Some(n) = numero::<u64>(args, "relevo-reenganche").filter(|n| *n > 0) {
        Some(Relevo { cada_s: n, provisioned_s: None, reenganche: true })
    } else if let Some(n) = numero::<u64>(args, "relevo").filter(|n| *n > 0) {
        Some(Relevo { cada_s: n, provisioned_s: None, reenganche: nativo && sin_juego })
    } else if let Some(s) = provisioned_pedido.filter(|s| *s > 0 && nativo && sin_juego) {
        // En modo música, `--relevo-provisioned` solo ya pide el relevo.
        Some(Relevo { cada_s: s, provisioned_s: None, reenganche: true })
    } else {
        None
    };
    // A S s de Provisioned: lo pedido, o 57 en el relevo por re-enganche de la nativa.
    let relevo = relevo.map(|mut r| {
        r.provisioned_s = provisioned_pedido.or_else(|| (nativo && r.reenganche).then_some(PROVISIONED_MUSICA_S)).filter(|s| *s > 0);
        r
    });
    let musica = sin_juego && relevo.is_some();
    // La reserva: `--reserva-musica=MS` explícita manda (sin tapar, salvo que se
    // pida); en modo música, tapar con 3 s (`--sin-tapar`: 150 ms fijos).
    let reserva_musica: Option<u32> = numero(args, "reserva-musica");
    let tapar_ms: Option<u32> = numero(args, "tapar-relevo")
        .or_else(|| hay(args, "--tapar-relevo").then_some(3000))
        .or_else(|| (musica && reserva_musica.is_none()).then_some(3000))
        .filter(|_| !hay(args, "--sin-tapar"));
    let reserva_ms = tapar_ms.or(reserva_musica).or_else(|| musica.then_some(150)).or_else(|| numero(args, "reserva")).unwrap_or(20);
    let reserva_inicial = valor(args, "reserva-inicial").and_then(|v| {
        let (ms, s) = v.split_once(',')?;
        Some((ms.parse::<u32>().ok()?, s.parse::<u64>().ok()?))
    }).or_else(|| nativo.then_some((40, 20)))
        .filter(|(ms, s)| *ms > 0 && *s > 0)
        .filter(|_| reserva_musica.is_none() && tapar_ms.is_none() && !musica);
    let avisos = (relevo.is_none() && provisioned_pedido.is_some_and(|s| s > 0))
        .then(|| "--relevo-provisioned no hace nada sin --relevo=N ni --sin-entrada".to_owned()).into_iter().collect();
    let play_tras_relevo = (sin_juego && !hay(args, "--sin-play-tras-relevo")).then(|| lista(args, "play-tras-relevo", &[0, 1000]));
    let play_en_pausa = tapar_ms.is_some() && play_tras_relevo.is_some() && !hay(args, "--sin-play-en-pausa");
    let app_al_empezar = (musica && !hay(args, "--sin-app")).then(|| valor(args, "app").unwrap_or(SPOTIFY).to_owned());
    Modo {
        nativo, play_tras_relevo, play_en_pausa, app_al_empezar, sin_juego, musica, reserva_ms, tapar: tapar_ms.is_some(), reserva_inicial, avisos,
        reserva_fija: tapar_ms.is_some() || reserva_musica.is_some() || musica, relevo,
        play_al_empezar: (sin_juego && !hay(args, "--sin-play-al-empezar")).then(|| lista(args, "play-al-empezar", &[0, 3000])),
    }
}

/// los experimentos cerrados de la ruta nativa (reinicio de ICE,
/// latidos sin mando, acuses de audio) y dos ajustes de medir sin uso. Un acceso viejo que
/// aún los lleve no falla: el registro lo dice.
const RETIRADAS: &[&str] = &["reinicio-ice", "relevo-ice", "nominacion-normal", "reinicio-config", "reinicio-sin-ciclo", "reconectar-con-clave",
    "probar-pt", "latido-nativo", "datos-control", "sin-audioack", "audio-ack", "rechazar-entrada", "ack-config", "mudo-tras", "mudo-salvo", "sin-chat",
    "credenciales", "hilos",
    // medido: la confirmación al núcleo no pesa; se queda en 50 ms.
    "confirmar-ms"];

/// Las opciones retiradas que trae `args` (sin el `--`).
pub fn retiradas(args: &[String]) -> Vec<&str> {
    args.iter().filter_map(|a| {
        let nombre = a.strip_prefix("--")?;
        if nombre == "party-activa=no-mandar" { return Some(nombre) }
        RETIRADAS.iter().copied().find(|r| nombre == *r || nombre.strip_prefix(r).is_some_and(|resto| resto.starts_with('=')))
    }).collect()
}

#[cfg(test)]
mod pruebas {
    use super::*;

    fn m(args: &str) -> Modo { modo(&args.split_whitespace().map(str::to_owned).collect::<Vec<_>>(), true) }

    /// El modo música, pedido de todas las formas: re-enganche a 57 s de
    /// Provisioned, reserva de 3 s que tapa el hueco y Play por la nube.
    #[test]
    fn modo_musica_de_todas_las_formas() {
        for args in ["--sin-entrada --relevo=40", "--sin-mando --relevo=40", "--sin-entrada --relevo-provisioned", "--sin-mando --relevo-provisioned",
                     "--sin-entrada --relevo-reenganche=40", "--nativo --sin-entrada --relevo=40"] {
            let x = m(args);
            assert!(x.nativo && x.sin_juego && x.musica, "{args}");
            let r = x.relevo.clone().unwrap();
            assert!(r.reenganche, "{args}");
            assert_eq!(r.provisioned_s, Some(PROVISIONED_MUSICA_S), "{args}");
            assert_eq!((x.reserva_ms, x.tapar, x.reserva_inicial), (3000, true, None), "{args}");
            assert_eq!(x.play_tras_relevo, Some(vec![0, 1000]), "{args}");
            assert!(x.play_en_pausa, "{args}");
            assert_eq!(x.app_al_empezar.as_deref(), Some(SPOTIFY), "{args}");
        }
        // El acceso de música de antes del 44: con el número dado, ése.
        let x = m("--sin-entrada --relevo-provisioned=55");
        assert_eq!((x.musica, x.tapar, x.reserva_ms, x.relevo.unwrap().provisioned_s), (true, true, 3000, Some(55)));
    }

    #[test]
    fn modo_juego_y_variantes() {
        // Sin opciones: nativa, con entrada, sin relevo; reserva de 20 ms (40 los primeros 20 s).
        let x = m("");
        assert_eq!((x.nativo, x.sin_juego, x.musica, x.relevo.clone(), x.reserva_ms, x.tapar, x.reserva_inicial), (true, false, false, None, 20, false, Some((40, 20))));
        assert_eq!((x.play_tras_relevo, x.play_en_pausa), (None, false));
        assert!(!m("--sin-entrada --relevo=40 --sin-play-en-pausa").play_en_pausa);
        assert_eq!(m("--sin-entrada --relevo=40 --app=9ABC").app_al_empezar.as_deref(), Some("9ABC"));
        assert_eq!(m("--sin-entrada --relevo=40 --sin-app").app_al_empezar, None);
        assert_eq!(m("--relevo=40").app_al_empezar, None, "en juego, nunca");
        assert!(!m("--sin-entrada --relevo=40 --sin-tapar").play_en_pausa);
        // Relevo con entrada: sin re-enganche, sin tapar, desde el /play.
        let x = m("--relevo=40");
        assert_eq!((x.musica, x.tapar, x.relevo.unwrap()), (false, false, Relevo { cada_s: 40, provisioned_s: None, reenganche: false }));
        // `--relevo-provisioned` con entrada no pide relevo.
        let x = m("--relevo-provisioned=57");
        assert_eq!((x.relevo, x.avisos.len()), (None, 1));
        assert!(m("--sin-entrada --relevo-provisioned").avisos.is_empty());
        // Sin entrada pero sin relevo: no es modo música, pero hay Play al empezar.
        let x = m("--sin-entrada");
        assert_eq!((x.musica, x.tapar, x.reserva_ms), (false, false, 20));
        assert_eq!(x.play_al_empezar, Some(vec![0, 3000]));
        // WebRTC: sin re-enganche ni Provisioned.
        let x = m("--webrtc --sin-mando --relevo=40");
        assert_eq!((x.nativo, x.musica), (false, true));
        assert_eq!(x.relevo.unwrap(), Relevo { cada_s: 40, provisioned_s: None, reenganche: false });
        // Sin la ruta WebRTC en el exe, `--webrtc` no cambia nada.
        assert!(modo(&["--webrtc".to_owned()], false).nativo);
    }

    #[test]
    fn opciones_retiradas() {
        let a: Vec<String> = "--sin-entrada --relevo-ice=40 --mudo-tras=5 --party-activa=no-mandar --party-activa --audio-ack=3 --audio-acks --hilos=2 --relevo=40"
            .split_whitespace().map(str::to_owned).collect();
        assert_eq!(retiradas(&a), vec!["relevo-ice", "mudo-tras", "party-activa=no-mandar", "audio-ack", "hilos"]);
    }

    #[test]
    fn opciones_de_la_reserva() {
        assert_eq!((m("--sin-entrada --relevo=40 --sin-tapar").reserva_ms, m("--sin-entrada --relevo=40 --sin-tapar").tapar), (150, false));
        let x = m("--sin-entrada --relevo=40 --reserva-musica=500");
        assert_eq!((x.reserva_ms, x.tapar, x.reserva_fija), (500, false, true));
        assert!(!m("--reserva=60").reserva_fija);
        let x = m("--sin-entrada --relevo=40 --tapar-relevo=2500");
        assert_eq!((x.reserva_ms, x.tapar), (2500, true));
        assert_eq!(m("--sin-entrada --relevo-provisioned=0").relevo, None);
        assert_eq!(m("--sin-entrada --relevo=40 --relevo-provisioned=0").relevo.unwrap().provisioned_s, None);
        assert_eq!(m("--reserva=60").reserva_ms, 60);
        assert_eq!(m("--reserva-inicial=0,0").reserva_inicial, None);
    }
}
