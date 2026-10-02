# Instala el cliente Rust (un solo exe) y crea sus accesos en la carpeta «Xbox solo audio» del escritorio.
#   powershell -ExecutionPolicy Bypass -File tools\instalar-rust.ps1 -Exe <ruta al exe> [-Consola <id>]
# El exe sale de GitHub Actions: gh run download <id> -D salida (rust/README.md).
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    # Opcional: sin ella, el cliente usa la primera consola encendida de la cuenta.
    [ValidatePattern('^[A-Za-z0-9_-]*$')][string]$Consola = '',
    # Ganancia de la salida en % (120 por defecto; por encima de 100, limitador suave).
    [ValidateRange(10, 400)][int]$Volumen = 120
)

if (-not (Test-Path -LiteralPath $Exe)) { Write-Error "No existe: $Exe"; exit 1 }
$destino = Join-Path $env:LOCALAPPDATA 'Programs\XboxSoloAudioRust'
$instalado = Join-Path $destino 'xbox-solo-audio.exe'

# Si está sonando, se le pide parar por su tubería (hace el DELETE a la consola)
# y se espera a que salga antes de sustituir el exe.
if (Get-Process -Name 'xbox-solo-audio' -ErrorAction SilentlyContinue) {
    try {
        $tuberia = New-Object System.IO.Pipes.NamedPipeClientStream('.', 'xbox-solo-audio-rust', [System.IO.Pipes.PipeDirection]::Out)
        $tuberia.Connect(3000)
        $bytes = [Text.Encoding]::ASCII.GetBytes("parar`n")
        $tuberia.Write($bytes, 0, $bytes.Length)
        $tuberia.Dispose()
    } catch { }
    for ($i = 0; $i -lt 50 -and (Get-Process -Name 'xbox-solo-audio' -ErrorAction SilentlyContinue); $i++) { Start-Sleep -Milliseconds 200 }
    if (Get-Process -Name 'xbox-solo-audio' -ErrorAction SilentlyContinue) { Write-Error 'xbox-solo-audio.exe sigue abierto: ciérralo y repite'; exit 1 }
}

New-Item -ItemType Directory -Force -Path $destino | Out-Null
Copy-Item -LiteralPath $Exe -Destination $instalado -Force
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'recursos\solo-audio.ico') -Destination (Join-Path $destino 'solo-audio.ico') -Force

# Los accesos van a la carpeta «Xbox solo audio» del escritorio, con un LÉEME que
# dice qué hace cada uno y cuál usar. Los dos por la nativa desde el 02-10 por la tarde:
# 32 min de juego limpios, y lo que se oculta es jitter igual que por WebRTC+RED
#. El exe del producto ya no trae WebRTC.
$escritorio = [Environment]::GetFolderPath('Desktop')
$carpeta = Join-Path $escritorio 'Xbox solo audio'
New-Item -ItemType Directory -Force -Path $carpeta | Out-Null
$shell = New-Object -ComObject WScript.Shell
function Nuevo-Acceso([string]$Nombre, [string]$Destino, [string]$Argumentos, [string]$Descripcion, [string]$Icono) {
    $a = $shell.CreateShortcut((Join-Path $carpeta "$Nombre.lnk"))
    $a.TargetPath = $Destino
    $a.Arguments = $Argumentos
    $a.WorkingDirectory = Split-Path $Destino
    if ($Icono) { $a.IconLocation = "$Icono,0" }
    $a.Description = $Descripcion
    $a.Save()
    "acceso «$Nombre» $Argumentos"
}
$icono = Join-Path $destino 'solo-audio.ico'
$conConsola = ((@($(if ($Consola) { "--consola=$Consola" }), $(if ($Volumen -ne 100) { "--volumen=$Volumen" })) | Where-Object { $_ }) -join ' ')
"instalado: $instalado ($([math]::Round((Get-Item $instalado).Length / 1MB, 2)) MiB)"
Nuevo-Acceso '1. JUGAR - audio de la Xbox en los cascos' $instalado $conConsola 'Para jugar: el audio de la consola en los auriculares del PC. Un clic suena, otro para.' $icono
# Modo música: sin entrada, la consola no entra
# en modo juego remoto; relevo por re-enganche a 57 s de Provisioned, Play por la
# nube y reserva de 3 s que tapa el hueco (todo lo decide modo.rs).
Nuevo-Acceso '2. MÚSICA - Spotify de la consola en los cascos' $instalado ((@('--sin-entrada', '--relevo-provisioned', $conConsola) | Where-Object { $_ }) -join ' ') 'Para Spotify u otra app de música de la consola. Un clic suena, otro para.' $icono
$greenlight = Join-Path $env:LOCALAPPDATA 'Programs\greenlight-desktop\Greenlight.exe'
if (Test-Path $greenlight) { Nuevo-Acceso '3. Greenlight - ver la consola' $greenlight '' 'Greenlight completo (imagen y mando). No hace falta para los otros dos.' '' }
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'recursos\LEEME-escritorio.txt') -Destination (Join-Path $carpeta 'LÉEME.txt') -Force
# Los accesos sueltos de antes del 02-10 (fuera de la carpeta).
foreach ($viejo in 'Xbox solo audio (Rust).lnk', 'Xbox solo audio (música).lnk', (Join-Path 'Xbox solo audio' '3. Greenlight - iniciar sesión y ver la consola.lnk')) {
    $f = Join-Path $escritorio $viejo
    if (Test-Path -LiteralPath $f) { Remove-Item -LiteralPath $f; "quitado el acceso suelto «$viejo»" }
}
