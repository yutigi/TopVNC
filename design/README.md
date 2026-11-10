# Local interactive UI previews

From the repository root, with Node.js installed:

```powershell
node scripts/preview-ui.mjs
```

Open <http://127.0.0.1:4173/> to choose a preview:

- **Desktop:** <http://127.0.0.1:4173/desktop/>. Click the simulated editor and
  type with your keyboard. Try Settings, display size, quality, Debug / Stats,
  disconnect/reconnect, Compact laptop, and full viewport.
- **Mobile:** <http://127.0.0.1:4173/mobile/>. Try Portrait/Landscape, Keys,
  Type, Pointer, Customize, language switching, and the FPS preset. Use
  **Try full viewport** to explore the interface without the design-lab frame.

These are the existing interactive design prototypes. Sessions, diagnostics,
and remote input are simulated; no VNC host or credentials are needed. The
production client remains in `web/`. Preferences are saved in this browser.

The preview server binds only to `127.0.0.1`, serves only prototype assets, and
requires no package installation or build. It does not publish either design.
Stop it with Ctrl+C. If port 4173 is occupied, choose another port:

```powershell
$env:TOPVNC_UI_PORT = '4174'
node scripts/preview-ui.mjs
```
