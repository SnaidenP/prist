# Prist — Especificación de construcción

> Documento para entregarle a una IA de coding (Claude Code u otra) como brief completo de lo que hay que construir. Incluye stack, arquitectura, estructura de directorios, comandos, y roadmap por fases.

## 0. Resumen del producto

Prist es un gestor de versiones de Flutter (como `fvm` / `puro`) escrito **100% en Rust**, distribuido como binario estático sin dependencias. Objetivo: instalar y cambiar de versión de Flutter más rápido que Puro, usando menos disco, sin las inestabilidades que Puro tiene por su uso agresivo de symlinks internos.

Diferenciadores frente a Puro/FVM:
- Deduplicación de objetos git vía **git alternates** (no symlinks profundos dentro del SDK).
- **Directory Junctions** en Windows en vez de symlinks — no requiere modo desarrollador ni admin.
- Descargas paralelas async con verificación de integridad.
- Escritura atómica de archivos de estado (nunca dejar el entorno en estado corrupto a mitad de una operación).

## 1. Stack técnico

| Área | Crate/herramienta | Por qué |
|---|---|---|
| Git | `gitoxide` (`gix`) | Puro en Rust, sin bindings a C, compila binarios estáticos, multihilo seguro |
| Runtime async | `tokio` | Descargas paralelas, IO no bloqueante |
| HTTP client | `reqwest` | Descarga de releases.json y engine artifacts |
| CLI | `clap` (derive) | Subcomandos tipados, ayuda automática |
| Autocompletado | `clap_complete` | Bash/Zsh/Fish/PowerShell |
| Progreso en terminal | `indicatif` | Barras de progreso multi-tarea |
| Serialización | `serde` + `serde_json` | Parseo de releases.json y config local |
| Windows junctions | `windows-sys` (o crate `junction`) | Directory Junctions sin symlinks |
| Self-update | `self_update` | Actualización del propio binario vía GitHub Releases |
| Logging/diagnóstico | `tracing` | Instrumentación para debug y flamegraphs |
| Benchmarks internos | `hyperfine` (CI, no runtime) | Medir tiempos de instalación cold/warm cache |

Targets de compilación:
- `x86_64-pc-windows-msvc` (+ explorar `aarch64-pc-windows-msvc`)
- `x86_64-apple-darwin` y `aarch64-apple-darwin` (universal binary)
- `x86_64-unknown-linux-musl` (estático, sin depender de glibc)
- `aarch64-unknown-linux-gnu` (edge/ARM)

## 2. Estructura de directorios en disco (runtime)

```
~/.prist/                          (Unix) | %LOCALAPPDATA%\prist\ (Windows)
├── shared/
│   ├── git_bare.git/              # repo bare central, alternates de todos los entornos
│   └── engines/
│       └── <engine_hash>/         # artifacts del engine, indexado por hash de commit
├── envs/
│   ├── <nombre_entorno>/          # clon deduplicado (usa alternates hacia git_bare.git)
│   └── default/                   # junction/symlink al entorno marcado como global
```

Config por proyecto: archivo `.pristrc` en la raíz del repo del usuario (equivalente a `.fvmrc`), con la versión/canal deseado.

Resolución de contexto: al ejecutar `prist flutter ...`, subir el árbol de directorios desde el CWD buscando `.pristrc`; si no se encuentra ninguno hasta la raíz del disco, usar `envs/default`.

## 3. Comandos requeridos (paridad con Puro/FVM)

Cada entorno se identifica por un **nombre elegido por el usuario**, no por la versión en sí — así se pueden tener varios entornos con nombres ligados a proyectos o propósitos, incluso si comparten versión de Flutter:

```
prist create music_app 3.0.1        # entorno "music_app" → Flutter 3.0.1
prist create nomina_rd 3.19.0       # entorno "nomina_rd" → Flutter 3.19.0
prist create beta_test beta         # entorno "beta_test" → canal beta

prist use music_app                 # activa "music_app" en el proyecto actual
prist ls                            # lista: music_app (3.0.1), nomina_rd (3.19.0), beta_test (beta)
```

Internamente el nombre es la clave del directorio en `envs/<nombre>/`; la versión/canal/commit resuelto queda guardado como metadato del entorno (no como parte del nombre), así que renombrar la referencia de versión en el futuro (`prist upgrade <nombre> <nueva_version>`, si se implementa) no rompe el nombre que el usuario ya está usando en sus `.pristrc`.

| Comando | Comportamiento |
|---|---|
| `prist create <nombre> [version\|channel\|commit]` | Le pone un **nombre propio** (definido por el usuario) a esa instalación de Flutter y la asocia a una versión/canal/commit. Resuelve la referencia contra el feed de releases, clona vía gitoxide con alternates hacia el bare central, descarga/enlaza el engine correspondiente. Ej: `prist create music_app 3.0.1` crea el entorno `music_app` apuntando a Flutter 3.0.1 |
| `prist use <id> [--global/-g]` | Escribe `.pristrc` (o la config global), actualiza el enlace del entorno activo, dispara la mutación de config de IDE |
| `prist ls` | Lista entornos instalados, marcando cuál es global y cuál está activo en el proyecto actual |
| `prist releases` | Muestra tabla paginada del feed remoto de versiones/canales |
| `prist rm <id>` | Elimina un entorno local (no toca el repo bare compartido) |
| `prist clean` | Quita la configuración de Prist del proyecto actual |
| `prist flutter / dart / pub <args>` | Proxy transparente: resuelve el entorno activo, ajusta `PATH`/`FLUTTER_ROOT`, y delega el proceso |
| `prist doctor` / `prist repair` | Verifica integridad del repo bare y de los alternates; reconstruye si detecta objetos faltantes (mitigación del riesgo de `git gc`) |
| `prist update` | Self-update del binario vía `self_update` |

## 4. Lógica interna que debe implementarse

### 4.1 Resolución de versiones
- Descargar y parsear `releases_<linux|macos|windows>.json` desde Google Cloud Storage.
- Modelo `serde` tolerante a campos desconocidos (no debe romperse si Google agrega campos nuevos).
- Mapear canal/versión semántica → hash de commit exacto.

### 4.2 Clonado y deduplicación
- Mantener un único repo bare en `shared/git_bare.git/`.
- Cada nuevo entorno se crea con gitoxide y se le escribe `.git/objects/info/alternates` apuntando al bare central.
- **Desactivar auto-prune / gc automático** sobre el bare central (`gc.auto 0`) para no invalidar los alternates de los entornos derivados.
- `prist doctor` debe poder detectar objetos faltantes y reconstruir el estado.

### 4.3 Engine de Flutter
- Leer `bin/internal/engine.version` del entorno recién clonado para obtener el hash del engine.
- Descargar el artifact correspondiente de forma paralela (tokio) mientras se resuelve el código fuente.
- Almacenar el engine en `shared/engines/<hash>/`, nunca duplicado dentro de cada entorno.
- Enlazar (junction en Windows, symlink en Unix) desde `envs/<id>/bin/cache/` hacia el engine compartido.
- Verificación de checksum tras la descarga.
- Escritura atómica de archivos de estado tipo `engine.stamp` (escribir a archivo temporal + rename), para evitar corrupción en escenarios de monorepo con builds concurrentes.

### 4.4 Symlinks/Junctions multiplataforma
- Unix: `std::os::unix::fs::symlink`.
- Windows: Directory Junctions vía `windows-sys` o crate `junction` — **nunca** symlinks (evita requerir modo desarrollador/admin).
- Nunca usar hardlinks de directorio (NTFS no lo permite).

### 4.5 Integración con IDEs
- **VS Code**: mutar `.vscode/settings.json` inyectando `dart.flutterSdkPath`, agregando exclusiones de watcher/search sobre las carpetas de Prist. Usar un parser tolerante a JSONC (comentarios) para no corromper configuración existente del usuario.
- **Android Studio / IntelliJ**: sobrescribir `flutter.sdk` en `android/local.properties`; inyectar `<ignored-roots>` en los XML de workspace de `.idea/` para evitar reindexado agresivo.
- Añadir entradas al `.gitignore` del proyecto para no commitear los artefactos internos de Prist.

## 5. Requisitos no funcionales

- **Rendimiento objetivo**: instalación en frío más rápida que Puro; instalación en caliente (entorno ya cacheado) debe acercarse a O(1).
- **Resiliencia**: ninguna operación debe dejar el sistema en estado a medio terminar — usar escritura atómica (temp file + rename) en todos los archivos de control.
- **Sin privilegios elevados**: nunca debe requerir admin/UAC en Windows ni sudo en Unix para uso normal.
- **Diagnosticabilidad**: `tracing` integrado para poder generar flamegraphs y debug de I/O.

## 6. Roadmap por fases (para pedirle a la IA que lo construya incremental)

**Fase 1 — MVP (Unix only, CLI básica)**
- Clonado vía gitoxide sin dedup todavía (clon directo).
- Parseo del feed de releases y resolución de versión → commit.
- Comandos: `create`, `use`, `ls`, `flutter` proxy. Solo Linux/macOS.

**Fase 2 — Dedup y motor concurrente (el diferenciador real)**
- Implementar el bare repo central + `alternates`.
- Descarga concurrente del engine con tokio + verificación de checksums.
- Escritura atómica de archivos de stamp/estado.
- Comando `prist doctor`.

**Fase 3 — Windows y integración de IDEs**
- Directory Junctions vía `windows-sys`.
- Mutación de `.vscode/settings.json` y `.idea/`/`local.properties`.
- `.gitignore` automático.

**Fase 4 — Feature-complete**
- `self_update`.
- Autocompletado de shell (`clap_complete`).
- Empaquetado/distribución: scripts `curl | bash` y PowerShell, Homebrew tap, `cargo install`.
- Notarización de binarios en macOS.
- Suite de benchmarks en CI (`hyperfine`) comparando contra Puro/FVM en tiempo, disco y ancho de banda.

## 7. Riesgos a tener presentes durante la construcción

- `git gc` sobre el bare central puede invalidar los alternates de todos los entornos → mitigar con `gc.auto 0` + `prist doctor` como red de seguridad.
- Cambios no anunciados en el esquema de `releases_*.json` de Google → deserialización tolerante a campos desconocidos, nunca asumir un esquema rígido.
- Las herramientas internas de Dart (AOT compiler, frontend_server) pueden no tolerar bien rutas resueltas vía enlaces si se anida demasiado profundo — evitar symlinks/junctions dentro de subcarpetas profundas del SDK; enlazar a nivel de `bin/cache/` únicamente, no más abajo.
