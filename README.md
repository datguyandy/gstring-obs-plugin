# GString Captions — Native OBS Audio Filter Plugin

Captures audio directly from any OBS Studio audio source (Microphone, Desktop Audio, Discord via Application Audio Capture, etc.) and streams gated 16 kHz mono PCM to GString's `/overlay/ws` endpoint — replacing the browser capture tab completely while keeping your existing OBS Browser Source and Caption Studio styling.

## What it does

- **Zero browser tabs:** Lives directly inside OBS as an audio filter (`GString Captions`).
- **Any OBS audio source:** Drop it on your Mic, Desktop Audio, or an isolated Application Audio Capture source (works on Windows, macOS, and Linux).
- **Auto-starts with your stream:** Defaults to **Only while streaming** so idle OBS sessions never burn caption minutes (configurable to **While streaming or recording** or **Always**).
- **Respects OBS mute:** Muting your source in OBS immediately pauses audio capture and closes the speech recognition stream.
- **TLS & mask-safe auth:** Paste a single revocable **OBS plugin key** from **Dashboard → Captions → OBS setup** into the password-masked field. The plugin enforces `wss://` (TLS) for all remote servers, sends credentials only in the `Authorization: Bearer` header, and revokes keys immediately when you click **Reset links** in the dashboard.
- **Zero extra runtime DLLs:** Resolves `libobs` / `obs-frontend-api` symbols from the host OBS process and uses `rustls` (no OpenSSL or OBS SDK download required to build).

## Install from GitHub Releases

Download the latest build from **Releases**:

- **Windows (OBS Studio 30+, 64-bit):** Run `gstring-captions-windows-x64-setup.exe`, or extract `gstring-captions-windows-x64.zip` into `C:\ProgramData\obs-studio\plugins\`.
- **macOS (OBS Studio 30+, Apple Silicon & Intel):** Extract `gstring-captions-macos-universal.zip` and move `gstring-captions.plugin` into `~/Library/Application Support/obs-studio/plugins/`.
- **Linux (OBS Studio 30+, x64):** Extract `gstring-captions-linux-x64.tar.gz` into `~/.config/obs-studio/plugins/`.

## Setup in OBS

1. In the GString dashboard, go to **Captions → OBS setup**.
2. Add the **Browser source URL** to your OBS scene (if you haven't already).
3. Switch **Audio capture** to **OBS plugin** and click **Copy key**.
4. In OBS's **Audio Mixer**, click the **⋮** menu on your Microphone (or any audio source) → **Filters** → **+** → **GString Captions**.
5. Paste the key into **OBS plugin key** and close the dialog.

> **Security note:** OBS masks password fields on screen, but stores filter settings in plaintext inside your local OBS scene-collection JSON file. Don't share raw exported scene collections without clearing the key first (or click **Reset links** in the GString dashboard to revoke it).

## Build from source

Requires Rust (`rustup`, stable toolchain).

```bash
cargo build --release
```

Run tests:

```bash
cargo test
```

On Windows, you can build and install locally in one step:

```powershell
.\install-windows.ps1
```

## Publishing a release (for maintainers)

1. Bump `version` in `Cargo.toml` (and `Cargo.lock`) and commit to `main`.
2. Tag and push:
   ```bash
   git tag v0.1.1
   git push origin v0.1.1
   ```
3. `.github/workflows/release.yml` runs tests, compiles Windows (`.exe` installer + `.zip`), macOS (Universal `.plugin.zip`), and Linux (`.tar.gz`), generates `SHA256SUMS.txt` and signed GitHub build provenance attestations, and publishes them to GitHub Releases.
4. Set `NEXT_PUBLIC_OBS_PLUGIN_URL=https://github.com/gstringbot/obs-plugin/releases/latest` in the GString dashboard's Vercel environment variables (one-time).

## License

MIT — see [LICENSE](LICENSE).
