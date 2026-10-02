//! Autenticación sin Node: la ruta `Msal` de xal-node 1.1.6-beta2 que usa
//! el prototipo en Node (esta ruta no firma peticiones).
//!
//!   1. token de usuario: refresh_token → login.microsoftonline.com (se guarda: rota)
//!   2. user.auth.xboxlive.com (RPS) → 3. XSTS para http://gssv.xboxlive.com/
//!   4. xhome.gssv-play-prod.xboxlive.com/v2/login/user → gsToken y región
//!   5. login.live.com → lpt (el token de /connect)
//!   6. sin `--consola`: xccs, la primera consola encendida; 7. si ninguna lo está, Power/WakeUp
//!
//! la sesión es propia, `%LOCALAPPDATA%\XboxSoloAudio\cuenta.json`
//! (el refresh_token, que rota en cada uso: se guarda con cerrojo y renombrado).
//! Si no existe, se importa una vez la de Greenlight (%APPDATA%\greenlight-desktop\
//! config.json, clave user.tokenstore, JSON dentro de JSON; sólo se lee); si
//! tampoco hay, inicio de sesión por código de dispositivo (microsoft.com/link).
//! Ningún valor se imprime.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::http::{Http, Respuesta};

use crate::xhome::Credenciales;

type Resultado<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const CLIENT_ID: &str = "1f907974-e22b-4810-a9de-d9647380c97e";
const TOKEN: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
const ALCANCE: &str = "xboxlive.signin openid profile offline_access";
const XBL: [(&str, &str); 3] = [("x-xbl-contract-version", "1"), ("Origin", "https://www.xbox.com"), ("Referer", "https://www.xbox.com/")];

pub fn ruta_config() -> PathBuf {
    PathBuf::from(std::env::var("APPDATA").unwrap_or_default()).join("greenlight-desktop").join("config.json")
}

/// `elegir_consola`: además, busca la consola de la cuenta (la primera encendida)
/// como hace Greenlight (xbox-webapi smartglass: xccs.xboxlive.com/lists/devices).
pub async fn autenticar(elegir_consola: bool) -> Resultado<Credenciales> {
    let http = Http::nuevo(Duration::from_secs(15), false)?;
    // 1. Token de usuario nuevo. El de refresco rota en cada uso: leer, renovar y
    // guardar van bajo el mismo cerrojo (el relevo autentica en otro hilo a la vez).
    let cerrojo = Cerrojo::tomar()?;
    let usuario: Value = match refresco_guardado() {
        Some(refresco) => {
            let cuerpo = formulario_cuerpo(&[("client_id", CLIENT_ID), ("grant_type", "refresh_token"), ("refresh_token", &refresco), ("scope", ALCANCE)]);
            let r = http.pedir("POST", TOKEN, &[("Content-Type", "application/x-www-form-urlencoded")], Some(cuerpo)).await?;
            if r.bien() { serde_json::from_str(&r.cuerpo)? }
            // Sesión caducada o revocada: se olvida y se pide el código otra vez.
            else if r.cuerpo.contains("invalid_grant") {
                crate::registro::registrar(Instant::now(), "sesión: la guardada ya no vale (invalid_grant); hay que iniciar sesión otra vez");
                let _ = std::fs::remove_file(ruta_cuenta());
                drop(cerrojo);
                let u = iniciar_sesion(&http).await?;
                let c = Cerrojo::tomar()?;
                c.guardar(u["refresh_token"].as_str().ok_or("sin refresh_token")?)?;
                return terminar(&http, u, elegir_consola).await;
            } else { return Err(format!("autenticación: {TOKEN} → {}", r.estado).into()) }
        }
        None => { drop(cerrojo); let u = iniciar_sesion(&http).await?; let c = Cerrojo::tomar()?; c.guardar(u["refresh_token"].as_str().ok_or("sin refresh_token")?)?; return terminar(&http, u, elegir_consola).await }
    };
    cerrojo.guardar(usuario["refresh_token"].as_str().ok_or("sin refresh_token")?)?;
    drop(cerrojo);
    terminar(&http, usuario, elegir_consola).await
}

/// Del token de usuario en adelante (Xbox Live, XSTS, xHome, lpt, consola).
async fn terminar(http: &Http, usuario: Value, elegir_consola: bool) -> Resultado<Credenciales> {
    let acceso = usuario["access_token"].as_str().ok_or("sin access_token")?;
    let refresco = usuario["refresh_token"].as_str().unwrap_or_default().to_owned();

    // 2-3. Xbox Live y XSTS para el servicio de streaming.
    let xasu = jsonp(&http, "https://user.auth.xboxlive.com/user/authenticate", &XBL, json!({
        "Properties": { "AuthMethod": "RPS", "RpsTicket": format!("d={acceso}"), "SiteName": "user.auth.xboxlive.com" },
        "RelyingParty": "http://auth.xboxlive.com", "TokenType": "JWT",
    })).await?;
    let xsts = jsonp(&http, "https://xsts.auth.xboxlive.com/xsts/authorize", &XBL, json!({
        "Properties": { "SandboxId": "RETAIL", "UserTokens": [xasu["Token"].as_str().ok_or("sin token de Xbox Live")?] },
        "RelyingParty": "http://gssv.xboxlive.com/", "TokenType": "JWT",
    })).await?;

    // 4. Token de juego en casa y región.
    let xhome = jsonp(&http, "https://xhome.gssv-play-prod.xboxlive.com/v2/login/user", &[("x-gssv-client", "XboxComBrowser")],
        json!({ "token": xsts["Token"].as_str().ok_or("sin token XSTS")?, "offeringId": "xhome" })).await?;
    let region = xhome["offeringSettings"]["regions"].as_array().and_then(|r| r.iter().find(|r| r["isDefault"] == true))
        .ok_or("sin región predeterminada")?;
    let host = region["baseUri"].as_str().ok_or("sin baseUri")?.trim_start_matches("https://").trim_end_matches('/').to_owned();

    // 5. lpt para /connect.
    let lpt = formulario(&http, "https://login.live.com/oauth20_token.srf", &[
        ("client_id", CLIENT_ID), ("scope", "service::http://Passport.NET/purpose::PURPOSE_XBOX_CLOUD_CONSOLE_TRANSFER_TOKEN"),
        ("grant_type", "refresh_token"), ("refresh_token", &refresco),
    ]).await?;

    // 6. XSTS web (para xccs: lista de consolas, encender, Media/Play tras un
    // relevo) y la consola, si no la dieron.
    let web = jsonp(&http, "https://xsts.auth.xboxlive.com/xsts/authorize", &XBL, json!({
        "Properties": { "SandboxId": "RETAIL", "UserTokens": [xasu["Token"].as_str().ok_or("sin token de Xbox Live")?] },
        "RelyingParty": "http://xboxlive.com", "TokenType": "JWT",
    })).await?;
    let uhs = web["DisplayClaims"]["xui"][0]["uhs"].as_str().ok_or("sin uhs")?;
    let token = web["Token"].as_str().ok_or("sin token web")?;
    let xccs = format!("XBL3.0 x={uhs};{token}");
    let mut encendida = false;
    let consola = if elegir_consola {
        let url = "https://xccs.xboxlive.com/lists/devices?queryCurrentDevice=false&includeStorageDevices=true";
        let autorizacion = format!("XBL3.0 x={uhs};{token}");
        let lista = respuesta(url, http.pedir("GET", url, &xccs_cabeceras(&autorizacion), None).await?)?;
        let consolas = lista["result"].as_array().cloned().unwrap_or_default();
        let elegida = consolas.iter().find(|c| c["powerState"] == "On").or(consolas.first());
        let id = elegida.and_then(|c| c["id"].as_str()).map(str::to_owned);
        // 7. Si ninguna está encendida, se enciende la primera (en reposo
        // «ConnectedStandby»; con el ahorro de energía no responde) y se espera
        // a verla «On», como el botón de Greenlight (xbox-webapi, smartglass.js).
        if let (Some(id), Some(c)) = (&id, elegida) {
            if c["powerState"] != "On" {
                let autorizacion = format!("XBL3.0 x={uhs};{token}");
                encender(&http, &autorizacion, id).await?;
                encendida = true;
            }
        }
        id
    } else { None };

    Ok(Credenciales {
        host,
        region: region["name"].as_str().unwrap_or("").to_owned(),
        gs_token: xhome["gsToken"].as_str().ok_or("sin gsToken")?.to_owned(),
        lpt: lpt["access_token"].as_str().ok_or("sin lpt")?.to_owned(),
        consola,
        encendida,
        xccs: Some(xccs),
    })
}

/// ¿está encendida la consola (`powerState` «On»)? Sin `consola`,
/// cualquiera de la cuenta. Sólo mira: no la enciende (si la apagó el usuario,
/// que siga apagada hasta que la encienda él).
pub async fn consola_encendida(autorizacion: &str, consola: Option<&str>) -> Resultado<bool> {
    let url = "https://xccs.xboxlive.com/lists/devices?queryCurrentDevice=false&includeStorageDevices=true";
    let lista = respuesta(url, http_xccs()?.pedir("GET", url, &xccs_cabeceras(autorizacion), None).await?)?;
    let consolas = lista["result"].as_array().cloned().unwrap_or_default();
    Ok(consolas.iter().any(|c| consola.is_none_or(|id| c["id"] == id) && c["powerState"] == "On"))
}

/// Una orden `Media` por xccs (Play, Pause…), como el prototipo en Node.
/// Devuelve el estado HTTP. `autorizacion` = `Credenciales::xccs`.
/// Un cliente para todas las órdenes (revisión 19 §2.3): conexión y TLS
/// reutilizados en vez de un apretón de manos en cada relevo. las
/// conexiones ociosas se sueltan a los 90 s (más que un relevo, ~57 s; una
/// semimuerta tras un corte de NAT no se queda para siempre) y keepalive TCP.
fn http_xccs() -> Resultado<&'static Http> {
    static HTTP: std::sync::OnceLock<Http> = std::sync::OnceLock::new();
    Ok(match HTTP.get() { Some(h) => h, None => { let h = Http::nuevo(Duration::from_secs(10), true)?; HTTP.get_or_init(|| h) } })
}

/// Las cabeceras de xccs (SmartGlass: lista de consolas y órdenes).
fn xccs_cabeceras(autorizacion: &str) -> [(&str, &str); 5] {
    [("Authorization", autorizacion), ("x-xbl-contract-version", "4"), ("skillplatform", "RemoteManagement"), ("Accept", "application/json"), ("Accept-Language", "en-US")]
}

/// abre (o refresca) las conexiones con xccs antes de que hagan
/// falta, para que el Play del relevo no pague DNS, TCP ni TLS. Lo que conteste
/// da igual. DOS a la vez. xccs va por
/// HTTP/1.1, una petición por conexión: el Play por tiempo de +0 ms ocupa una
/// ~1 s y el de la pausa, que sale ~20 ms después, abría otra en frío.
pub async fn calentar_xccs() -> Resultado<(u128, u128)> {
    let http = http_xccs()?;
    let una = || async { let inicio = std::time::Instant::now(); http.pedir("GET", "https://xccs.xboxlive.com/", &[], None).await.map(|_| inicio.elapsed().as_millis()) };
    let (a, b) = tokio::join!(una(), una());
    Ok((a?, b?))
}

pub async fn orden_media(autorizacion: &str, consola: &str, comando: &str) -> Resultado<String> {
    orden(autorizacion, consola, "Media", comando, json!([{}])).await
}

/// abre una app por su producto de la Tienda (`Shell`,
/// `ActivateApplicationWithOneStoreProductId`), como `xbox-nube.mjs`.
pub async fn abrir_app(autorizacion: &str, consola: &str, producto: &str) -> Resultado<String> {
    orden(autorizacion, consola, "Shell", "ActivateApplicationWithOneStoreProductId", json!([{ "oneStoreProductId": producto }])).await
}

/// qué hace la consola (`/consoles/<id>`): la app en primer plano
/// (`focusAppAumid`) y la reproducción (`playbackState`: Playing, Paused, Stopped…).
pub async fn estado_consola(autorizacion: &str, consola: &str) -> Resultado<(String, String)> {
    let url = format!("https://xccs.xboxlive.com/consoles/{consola}");
    let v = respuesta(&url, http_xccs()?.pedir("GET", &url, &xccs_cabeceras(autorizacion), None).await?)?;
    Ok((v["focusAppAumid"].as_str().unwrap_or("").to_owned(), v["playbackState"].as_str().unwrap_or("").to_owned()))
}

/// Una orden de SmartGlass por xccs; devuelve el estado HTTP y el resultado.
async fn orden(autorizacion: &str, consola: &str, tipo: &str, comando: &str, parametros: Value) -> Resultado<String> {
    let http = http_xccs()?;
    let cuerpo = json!({
        "destination": "Xbox", "type": tipo, "command": comando,
        "sessionId": uuid::Uuid::new_v4().to_string(), "sourceId": "com.microsoft.smartglass",
        "parameters": parametros, "linkedXboxId": consola,
    });
    let mut cabeceras = xccs_cabeceras(autorizacion).to_vec();
    cabeceras.push(("Content-Type", "application/json"));
    let r = http.pedir("POST", "https://xccs.xboxlive.com/commands", &cabeceras, Some(cuerpo.to_string())).await?;
    let estado = r.estado;
    let resultado = serde_json::from_str::<Value>(&r.cuerpo).ok().and_then(|v| v["result"].as_str().or(v["status"]["errorCode"].as_str()).map(str::to_owned)).unwrap_or_default();
    Ok(format!("{estado} {resultado}").trim().to_owned())
}

/// `Power/WakeUp` por xccs y espera (hasta 60 s) a que la lista la dé «On».
async fn encender(http: &Http, autorizacion: &str, id: &str) -> Resultado<()> {
    let url = "https://xccs.xboxlive.com/commands";
    let cuerpo = json!({
        "destination": "Xbox", "type": "Power", "command": "WakeUp",
        "sessionId": uuid::Uuid::new_v4().to_string(), "sourceId": "com.microsoft.smartglass",
        "parameters": [], "linkedXboxId": id,
    });
    let mut cabeceras = xccs_cabeceras(autorizacion).to_vec();
    cabeceras.push(("Content-Type", "application/json"));
    respuesta(url, http.pedir("POST", url, &cabeceras, Some(cuerpo.to_string())).await?)?;
    let lista = "https://xccs.xboxlive.com/lists/devices?queryCurrentDevice=false&includeStorageDevices=true";
    let limite = Instant::now() + Duration::from_secs(60);
    while Instant::now() < limite {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let estado = respuesta(lista, http.pedir("GET", lista, &xccs_cabeceras(autorizacion), None).await?)?;
        let encendida = estado["result"].as_array().into_iter().flatten()
            .any(|c| c["id"].as_str() == Some(id) && c["powerState"] == "On");
        if encendida { return Ok(()) }
    }
    Err("la consola no se enciende en 60 s (¿modo de ahorro de energía?)".into())
}

async fn formulario(http: &Http, url: &str, campos: &[(&str, &str)]) -> Resultado<Value> {
    let cuerpo = campos.iter().map(|(k, v)| format!("{}={}", codificar(k), codificar(v))).collect::<Vec<_>>().join("&");
    let cabeceras = [("Content-Type", "application/x-www-form-urlencoded"), ("Cache-Control", "no-store, must-revalidate, no-cache")];
    respuesta(url, http.pedir("POST", url, &cabeceras, Some(cuerpo)).await?)
}

async fn jsonp(http: &Http, url: &str, cabeceras: &[(&str, &str)], cuerpo: Value) -> Resultado<Value> {
    let mut todas = vec![("Content-Type", "application/json"), ("Cache-Control", "no-cache")];
    todas.extend_from_slice(cabeceras);
    respuesta(url, http.pedir("POST", url, &todas, Some(cuerpo.to_string())).await?)
}

/// El estado y la URL, nunca el cuerpo (lleva tokens).
fn respuesta(url: &str, r: Respuesta) -> Resultado<Value> {
    if !r.bien() { return Err(format!("autenticación: {url} → {}", r.estado).into()) }
    Ok(serde_json::from_str(&r.cuerpo)?)
}

fn codificar(texto: &str) -> String {
    texto.bytes().map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
        _ => format!("%{b:02X}"),
    }).collect()
}

fn leer_config(config: &Path) -> Resultado<Value> {
    serde_json::from_str(&std::fs::read_to_string(config)?).map_err(|_| "el config.json de Greenlight no contiene JSON válido".into())
}

fn leer_almacen(config: &Path) -> Resultado<Value> {
    let c = leer_config(config)?;
    let texto = c["user"]["tokenstore"].as_str().ok_or("no hay sesión de Greenlight: inicia sesión una vez en Greenlight")?;
    serde_json::from_str(texto).map_err(|_| "el tokenstore de Greenlight no contiene JSON válido".into())
}

/// El almacén propio: `%LOCALAPPDATA%\XboxSoloAudio\cuenta.json`.
pub fn ruta_cuenta() -> PathBuf {
    PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default()).join("XboxSoloAudio").join("cuenta.json")
}

/// El refresh_token: el propio o, la primera vez, el de Greenlight (sólo se lee).
fn refresco_guardado() -> Option<String> {
    let propio = std::fs::read_to_string(ruta_cuenta()).ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v["refresh_token"].as_str().map(str::to_owned));
    propio.or_else(|| leer_almacen(&ruta_config()).ok().and_then(|a| a["userToken"]["refresh_token"].as_str().map(str::to_owned)))
}

/// El cerrojo de `cuenta.json`: lo toma quien va a leer, renovar y guardar el
/// token (que rota), para que dos autenticaciones a la vez no se pisen.
struct Cerrojo(PathBuf);

impl Cerrojo {
    fn tomar() -> Resultado<Self> {
        let ruta = ruta_cuenta();
        if let Some(d) = ruta.parent() { std::fs::create_dir_all(d)? }
        let cerrojo = ruta.with_extension("json.lock");
        let inicio = Instant::now();
        loop {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&cerrojo) {
                Ok(_) => return Ok(Self(cerrojo)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Un cerrojo de más de 30 s es de un proceso que murió.
                    if std::fs::metadata(&cerrojo).and_then(|m| m.modified()).map(|t| t.elapsed().unwrap_or_default() > Duration::from_secs(30)).unwrap_or(false) {
                        let _ = std::fs::remove_file(&cerrojo);
                    }
                    if inicio.elapsed() > Duration::from_secs(20) { return Err("la sesión guardada está ocupada".into()) }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Guarda el refresh_token nuevo por renombrado (no se queda a medias).
    fn guardar(&self, refresco: &str) -> Resultado<()> {
        let ruta = ruta_cuenta();
        let temporal = ruta.with_extension(format!("json.{}.{:?}.tmp", std::process::id(), std::thread::current().id()).replace(['(', ')'], ""));
        std::fs::write(&temporal, json!({ "refresh_token": refresco }).to_string())?;
        std::fs::rename(&temporal, &ruta)?;
        Ok(())
    }
}

impl Drop for Cerrojo {
    fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); }
}

fn formulario_cuerpo(campos: &[(&str, &str)]) -> String {
    campos.iter().map(|(k, v)| format!("{}={}", codificar(k), codificar(v))).collect::<Vec<_>>().join("&")
}

/// Sin sesión: código de dispositivo. Se abre microsoft.com/link en el navegador
/// y una ventanita con el código (el exe no tiene consola); se sondea hasta que
/// el usuario lo escribe, como mucho lo que diga Microsoft (15 min).
async fn iniciar_sesion(http: &Http) -> Resultado<Value> {
    let d = formulario(http, "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode", &[("client_id", CLIENT_ID), ("scope", ALCANCE)]).await?;
    let codigo = d["user_code"].as_str().ok_or("sin user_code")?.to_owned();
    let dispositivo = d["device_code"].as_str().ok_or("sin device_code")?.to_owned();
    let url = d["verification_uri"].as_str().unwrap_or("https://www.microsoft.com/link").to_owned();
    let mut cada = d["interval"].as_u64().unwrap_or(5).max(1);
    let limite = Instant::now() + Duration::from_secs(d["expires_in"].as_u64().unwrap_or(900));
    crate::registro::registrar(Instant::now(), &format!("sesión: no hay ninguna guardada; inicia sesión en {url} con el código {codigo}"));
    avisar_codigo(&url, &codigo);
    while Instant::now() < limite {
        tokio::time::sleep(Duration::from_secs(cada)).await;
        let cuerpo = [("grant_type", "urn:ietf:params:oauth:grant-type:device_code"), ("client_id", CLIENT_ID), ("device_code", dispositivo.as_str())]
            .iter().map(|(k, v)| format!("{}={}", codificar(k), codificar(v))).collect::<Vec<_>>().join("&");
        let r = http.pedir("POST", TOKEN, &[("Content-Type", "application/x-www-form-urlencoded")], Some(cuerpo)).await?;
        if r.bien() { return Ok(serde_json::from_str(&r.cuerpo)?) }
        let error = serde_json::from_str::<Value>(&r.cuerpo).ok().and_then(|v| v["error"].as_str().map(str::to_owned)).unwrap_or_default();
        match error.as_str() {
            "authorization_pending" => {}
            "slow_down" => cada += 5,
            "authorization_declined" => return Err("inicio de sesión: rechazado en la página de Microsoft".into()),
            "expired_token" => return Err("inicio de sesión: el código caducó; vuelve a abrirlo".into()),
            otro => return Err(format!("inicio de sesión: {otro} ({})", r.estado).into()),
        }
    }
    Err("inicio de sesión: el código caducó sin usarse".into())
}

/// Abre la página en el navegador y enseña el código en una ventanita (en su
/// hilo: no bloquea el sondeo). Fuera de Windows, sólo el registro.
#[cfg(windows)]
fn avisar_codigo(url: &str, codigo: &str) {
    use windows::core::HSTRING;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, SW_SHOWNORMAL};
    unsafe { ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from(url), None, None, SW_SHOWNORMAL); }
    let texto = format!("Para oír tu Xbox, inicia sesión con tu cuenta de Microsoft:\n\n1. En el navegador que se acaba de abrir ({url})\n2. escribe este código:   {codigo}\n\nEn cuanto termines empezará a sonar solo. Sólo hace falta una vez.");
    std::thread::spawn(move || unsafe {
        MessageBoxW(None, &HSTRING::from(texto), &HSTRING::from("Iniciar sesión"), MB_OK | MB_ICONINFORMATION | MB_TOPMOST | MB_SETFOREGROUND);
    });
}

#[cfg(not(windows))]
fn avisar_codigo(_: &str, _: &str) {}
