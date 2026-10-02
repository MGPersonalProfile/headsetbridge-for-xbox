//! `--salida-iac3`: salida compartida de WASAPI con
//! `IAudioClient3::InitializeSharedAudioStream` y el periodo MÍNIMO que dé
//! `GetSharedModeEnginePeriod` (en muchos equipos 2,67-3 ms a 48 kHz), en vez
//! de los 10 ms de cpal. Sigue siendo modo compartido: Discord suena a la vez.
//! Un hilo propio, guiado por evento: en cada aviso se llena lo que cabe con la
//! misma `Relleno` que usa cpal.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use windows::core::GUID;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioClient3, IAudioRenderClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, DEVICE_STATE_ACTIVE, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY;

use crate::audio::{Relleno, ReservaCompartida, LLAMADA_NS, SALIDA_MAXIMA_US, SALIDA_ULTIMA_US};
use crate::registro::registrar;

type Resultado<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// PKEY_Device_FriendlyName (así no hace falta la característica FunctionDiscovery).
const NOMBRE: PROPERTYKEY = PROPERTYKEY { fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0), pid: 14 };
/// KSDATAFORMAT_SUBTYPE_IEEE_FLOAT.
const FLOTANTE: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xfffe;

/// Mientras exista, suena; al soltarlo, el hilo para.
pub struct Iac3 { parar: Arc<AtomicBool> }

impl Drop for Iac3 {
    fn drop(&mut self) { self.parar.store(true, Ordering::Relaxed) }
}

unsafe fn nombre(d: &IMMDevice) -> String {
    d.OpenPropertyStore(STGM_READ).and_then(|p| p.GetValue(&NOMBRE)).map(|v| v.to_string()).unwrap_or_default()
}

unsafe fn elegir(salida: &str) -> Resultado<(IMMDevice, String)> {
    let enumerador: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    if !salida.is_empty() {
        let todos = enumerador.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        for i in 0..todos.GetCount()? {
            let d = todos.Item(i)?;
            let n = nombre(&d);
            if n.to_lowercase().contains(&salida.to_lowercase()) { return Ok((d, n)) }
        }
    }
    let d = enumerador.GetDefaultAudioEndpoint(eRender, eConsole)?;
    let n = nombre(&d);
    Ok((d, n))
}

/// ¿Es el formato del motor flotante de 32 bits, estéreo y a 48 kHz?
unsafe fn es_f32_estereo_48k(f: *const WAVEFORMATEX) -> bool {
    let w = std::ptr::read_unaligned(f);
    let etiqueta = w.wFormatTag;
    let flotante = match etiqueta {
        WAVE_FORMAT_IEEE_FLOAT => true,
        WAVE_FORMAT_EXTENSIBLE => { let x = std::ptr::read_unaligned(f as *const WAVEFORMATEXTENSIBLE); let sub = x.SubFormat; sub == FLOTANTE }
        _ => false,
    };
    flotante && { w.nChannels } == 2 && { w.nSamplesPerSec } == 48_000 && { w.wBitsPerSample } == 32
}

/// Abre la salida y devuelve el guardián y el nombre del dispositivo. Los
/// errores de apertura vuelven aquí (para que `vigilar_salida` reintente); los
/// de después, marcan `rota`.
pub fn abrir(salida: &str, reserva: ReservaCompartida, t0: Instant, rota: Arc<AtomicBool>) -> Resultado<(Iac3, String)> {
    let parar = Arc::new(AtomicBool::new(false));
    let (listo_tx, listo_rx) = std::sync::mpsc::channel::<Result<String, String>>();
    let (salida, p) = (salida.to_owned(), parar.clone());
    std::thread::spawn(move || {
        let r = unsafe { sonar(&salida, reserva, t0, &p, &listo_tx) };
        if let Err(e) = r {
            if listo_tx.send(Err(e.to_string())).is_err() {
                // Ya estaba sonando: es una caída.
                registrar(t0, &format!("audio (IAudioClient3): error de salida: {e}"));
                rota.store(true, Ordering::Relaxed);
            }
        }
    });
    match listo_rx.recv() {
        Ok(Ok(nombre)) => Ok((Iac3 { parar }, nombre)),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => Err("el hilo de IAudioClient3 terminó sin abrir".into()),
    }
}

unsafe fn sonar(salida: &str, reserva: ReservaCompartida, t0: Instant, parar: &AtomicBool, listo: &std::sync::mpsc::Sender<Result<String, String>>) -> Resultado<()> {
    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    let (dispositivo, nombre) = elegir(salida)?;
    let cliente: IAudioClient3 = dispositivo.Activate(CLSCTX_ALL, None)?;
    let formato = cliente.GetMixFormat()?;
    let resultado = (|| -> Resultado<()> {
        if !es_f32_estereo_48k(formato) {
            let w = std::ptr::read_unaligned(formato);
            let (canales, hz, bits) = ({ w.nChannels }, { w.nSamplesPerSec }, { w.wBitsPerSample });
            return Err(format!("el motor no está en f32 estéreo a 48 kHz ({canales} canales, {hz} Hz, {bits} bits): se queda cpal").into());
        }
        let (mut defecto, mut fundamental, mut minimo, mut maximo) = (0u32, 0u32, 0u32, 0u32);
        cliente.GetSharedModeEnginePeriod(formato, &mut defecto, &mut fundamental, &mut minimo, &mut maximo)?;
        let ms = |f: u32| f as f64 / 48.0;
        registrar(t0, &format!("IAudioClient3: periodo por defecto {defecto} ({:.2} ms), fundamental {fundamental} ({:.2} ms), mínimo {minimo} ({:.2} ms), máximo {maximo} ({:.2} ms)",
            ms(defecto), ms(fundamental), ms(minimo), ms(maximo)));
        cliente.InitializeSharedAudioStream(AUDCLNT_STREAMFLAGS_EVENTCALLBACK, minimo, formato, None)?;
        let evento: HANDLE = CreateEventW(None, false, false, None)?;
        cliente.SetEventHandle(evento)?;
        let tamano = cliente.GetBufferSize()?;
        let mut actual = 0u32;
        let mut formato_actual: *mut WAVEFORMATEX = std::ptr::null_mut();
        if cliente.GetCurrentSharedModeEnginePeriod(&mut formato_actual, &mut actual).is_ok() && !formato_actual.is_null() { CoTaskMemFree(Some(formato_actual as _)); }
        registrar(t0, &format!("IAudioClient3: periodo en uso {actual} ({:.2} ms), búfer {tamano} ({:.2} ms)", ms(actual), ms(tamano)));
        let render: IAudioRenderClient = cliente.GetService()?;
        let mut relleno = Relleno::default();
        cliente.Start()?;
        let _ = listo.send(Ok(nombre.clone()));
        let fin = loop {
            if parar.load(Ordering::Relaxed) { break Ok(()) }
            if WaitForSingleObject(evento, 200) != WAIT_OBJECT_0 { continue }
            let inicio = Instant::now();
            let pendiente = match cliente.GetCurrentPadding() { Ok(p) => p, Err(e) => break Err(e.into()) };
            let libres = tamano.saturating_sub(pendiente);
            // Lo ya encolado es lo que tarda en sonar lo que se escribe ahora.
            let us = u64::from(pendiente) * 1_000_000 / 48_000;
            SALIDA_ULTIMA_US.store(us, Ordering::Relaxed);
            SALIDA_MAXIMA_US.fetch_max(us, Ordering::Relaxed);
            if libres == 0 { continue }
            let datos = match render.GetBuffer(libres) { Ok(d) => d, Err(e) => break Err(e.into()) };
            let muestras = std::slice::from_raw_parts_mut(datos as *mut f32, libres as usize * 2);
            relleno.llenar(muestras, &reserva, t0);
            if let Err(e) = render.ReleaseBuffer(libres, 0) { break Err(e.into()) }
            LLAMADA_NS.fetch_add(inicio.elapsed().as_nanos() as u64, Ordering::Relaxed);
        };
        let _ = cliente.Stop();
        let _ = CloseHandle(evento);
        fin
    })();
    CoTaskMemFree(Some(formato as _));
    resultado
}
