# Willow Voice reference research

Research date: 24 September 2026 (India); source capture began 23 September 2026 UTC. Installed version inspected: **2.1.19**. This document distinguishes packaged-code measurements, public product claims, supplied-video observations, and implementation recommendations. The installation was read only; no Willow account storage, credentials, transcript database, or user profile was inspected.

## Collected references and reproducibility

- `references/supplied/`: all **11 supplied PNG screenshots** and the original **11.1013-second MP4** copied byte-for-byte. Originals were preserved.
- `references/video-contact-sheet.jpg`: 2 fps contact sheet of the supplied 806 × 684, approximately 30 fps recording. Its audio is AAC stereo/48 kHz; the source was not muted or modified.
- `references/installed/out/renderer/assets/`: all available packaged app image/audio assets plus CSS and static bundled JavaScript for local inspection. No code from these bundles is imported into Utterly.
- `references/installed/embedded/`: decoded inline image/audio data URLs, deduplicated by SHA-256.
- `references/installed/asar-index.json`: full archive filename/size index. `package.json` gives app version; `size.json` records installation footprint.
- `references/web/`: official homepage, Windows use-case page, download, pricing, privacy policy, blog, help center, Getting Started and Troubleshooting collections and their linked articles; linked image, vector and WOFF2 assets; public release metadata and installer HEAD response.
- `references/manifest.json`, `references/web-and-embedded-manifest.json`, and `references/web/help-manifest.json`: source/path/byte length/SHA-256 provenance. Derived contact sheet and readable page text are additional analysis outputs.
- Re-run collection with `python scripts/research-willow.py`, `python scripts/research-willow-web.py`, and `python scripts/research-willow-details.py`. These use Python standard library only. The video contact sheet was generated with `ffmpeg -i <supplied video> -vf fps=2,scale=403:342,tile=5x5 -frames:v 1 <contact sheet>`.

The installed reference inventory currently contains **47 PNG, 41 WebP, 8 JPG, 2 JPEG, 9 SVG, 1 ICO and 6 MP3** files, including decoded inline assets. There are also 136 JS files, 5 CSS files and one HTML file. The initial web collection includes 96 PNG, 3 WebP, 3 JPG, 3 SVG and 81 WOFF2 URLs; some are responsive-size variants of the same image/font. Manifests retain exact provenance. One homepage asset request failed and is explicitly recorded, rather than silently presented as collected. Embedded vector components inside JavaScript are available as reference code; the complete app icon library was not converted into standalone SVG files.

These are **reference assets**, separate from the application bundle. The chosen shipped identity is the user's `icon.png`. A package-level MIT string alone is not evidence that every branded graphic, third-party logo, photograph, sound, or font has the same redistribution terms.

## Size and runtime architecture

| Item | Verified measurement |
| --- | --- |
| Installed version | 2.1.19 |
| Installed files | 33 filesystem files, recursively |
| Installed footprint | 336,503,336 bytes = 320.914 MiB |
| Main `Willow Voice.exe` | 221,991,632 bytes = 211.708 MiB |
| `resources/app.asar` | 38,747,135 bytes = 36.952 MiB |
| Public Windows installer | 101,412,096 bytes = 96.714 MiB |
| Public release | 2.1.19, releaseDate 2026-09-15T02:22:26.868Z |

Installer size was verified independently by the public `latest.yml` and HTTP `Content-Length` on the download link; the 101 MB installer was **not downloaded**. Installed footprint excludes profile data, optional downloaded offline models, filesystem allocation overhead and runtime RAM. RAM/CPU were not measured for Willow, so these are not runtime-memory claims.

The installed application is Electron with a React/CSS renderer, plus native modules for accessibility, audio capture, screenshot capture and taskbar information. It ships `whisper-server.exe` and a VAD helper. This explains much of its footprint; Utterly can reproduce its interaction visually with Rust/native Windows drawing without inheriting Electron.

Sources: `references/installed/package.json`, `references/installed/size.json`, `references/web/latest.yml`, `references/web/release-and-help.json`.

## Typography: desktop and website differ

**Desktop declared face:** the global CSS selector specifies `Inter Variable` first, followed by Apple system fonts, Segoe UI, Roboto, Oxygen, Ubuntu, Cantarell, Fira Sans, Droid Sans, Helvetica Neue, Arial and emoji fallbacks. Seven `@font-face` declarations describe variable Inter weights 100–900, normal style, with Cyrillic/Greek/Vietnamese/Latin subsets at relative `./files/inter-*-wght-normal.woff2` URLs.

**Important packaging observation:** none of those referenced WOFF2 files—or any TTF/OTF/WOFF file—exists anywhere in the installed ASAR index. The install directory also contains no corresponding font assets. The browser therefore cannot resolve these bundled relative font URLs in this inspected package. Windows **Segoe UI is the expected fallback**, unless another local font resolution path applies. This is a package observation/inference, not a browser `document.fonts` measurement. Use native Segoe UI for matching this Windows installation; use a separately licensed Inter distribution only if deliberately choosing the intended cross-platform face.

**Website:** the public homepage declares/downloads **Figtree** (Google Fonts URLs captured), along with additional font subsets/styles used by Framer and site components. Do not infer the desktop font from the marketing homepage. Website text and the actual installed app are different typography systems.

Desktop type tokens (logical pixels): extra-large40, large32, midtitle28, body17, h1 24, h2 22 or20, h3 17, h4 16, h5 15, paragraph14/13/12, small12/10. Settings rows actually use **14px medium titles**, **12px medium descriptions**, tight −0.025em tracking, and description line-height1.25. Main page headings use17px semibold. This is subtler than uniformly using large text.

Source: `references/installed/out/renderer/assets/index-UILcnHIf.css`, `SettingsRow-BLZHFYQX.js`, and website HTML/font downloads.

## Exact pill geometry and motion

All dimensions below are **CSS/logical pixels from the installed source**, not scaled promotional artwork.

| State/part | Value |
| --- | --- |
| Idle handle | 32 × 6, radius16, 0.75px white20% border |
| Recording/scribing capsule | 100 × 36, radius18 |
| Hands-free capsule | width135 |
| Surface | `#1e1e20`; 1px `rgba(255,255,255,.2)` border |
| Bottom gap | Native window6 + surface margin2 = 8 logical px above work-area bottom |
| Capsule content | height36, padding5px12px, gap8 |
| Focused-app icon | 18 × 18, contain, no forced rounding or white badge |
| Waveform | 11 bars; width2, gap2, baseheight18, rounded ends; container24 high |
| Loading indicator | 16 × 16 |
| Morph | 300ms `cubic-bezier(.2,0,0,1)` for width, height, radius, border and margin |
| Pill content entrance | opacity150ms ease-out after200ms |
| Pill content exit | hidden layer opacity switches immediately |
| Waveform/spinner crossfade | opacity150ms ease-out |
| Floating Scribe button | 28px circle, bottom36, center+45 horizontally |
| Floating button entrance | opacity120ms and scale150ms cubic(.34,1.56,.64,1), delayed200ms |

The marketing screenshots show substantially enlarged illustrations (for example approximately354 × 105 pixels in the supplied crop). They should **not** be used as desktop logical-size requirements. The user's idle screenshot is approximately40 × 8 physical pixels, consistent with32 × 6 at125% display scale. This scale explanation is inference, although it fits both screenshots and code.

The native Electron backing window is **525 × 500**, transparent, frameless, shadowless, non-resizable, non-focusable, always-on-top, skipped in the taskbar and mouse-transparent by default. Its visible surface anchors at bottom center. Utterly does not need an equally large backing window for the compact pill, but must remain nonactivating so the text insertion target is preserved.

The installed position tracker follows the monitor nearest the cursor every1,000ms. It refreshes native taskbar state every fifth tick, handles added/removed/changed displays, and debounces display-change positioning200ms. It uses the work area while the taskbar is visible and display bounds while hidden. Window x is `area.x + floor((area.width - 525)/2)` and y is `area.y + area.height - 500 - 6`. Automatic-positioning-disabled mode positions on the primary display.

Sources: `BarWindow-CFwiH_q-.css`, `BarWindow-CSj2Bb6f.js`, `out/main/index.js`; compact excerpts in `references/installed/layout-snippets.txt`.

## Microphone responsiveness and loading/buffer symbol

The waveform is driven by native microphone dB events. The exact normalization is `clamp((db + 60) / 26, 0, 1)`, then exponent1.15. It maintains a ten-value history. For bar index i, normalized distance from center e controls a history delay `round(e * 9)` and amplitude attenuation `1 - .55*e`. Consequently the center reacts first and tallest, and edges echo a softer delayed response.

Smoothed amplitude is `previous * .87^(dt*60) + target * (1-.87^(dt*60))`, with dt capped at0.1seconds. Display height is `18 * (.14 + .86*amplitude)` and opacity `.5 + .45*amplitude`. A subtle sine floor of at most0.06 adds life in quiet moments; reduced-motion disables that synthetic floor. Request-animation-frame stops when inactive. This is not a random equalizer: the user's actual microphone level drives the shape.

The **buffer/loading logo is a circular spinner, not the Willow tree mark**: SVG viewbox200 × 200, circle center100, radius70, stroke15 and rounded caps. A dim full ring has opacity0.2; the bright arc uses dasharray`200 1000` and a radial opacity gradient from1 to0. It renders at16px and rotates counterclockwise once every2seconds linearly. A native equivalent needs only an antialiased dim circle plus tapered arc.

Scribe mode is distinct: the border becomes a rotating conic gradient (`#e94cb1`, `#f5dcb0`, `#76d7f0`, `#b790e0`), 4second linear period, with subtle color glow. The floating wand button seen in the Edge screenshot identifies this mode; it is not a universal recording control.

## Recording behavior and video observations

The public Windows workflow explicitly says: click a text field, hold the shortcut and speak, release for text to appear. The supplied Windows UI shows **Ctrl + Win**. The help center hotkey article is Mac-oriented (fn), so its default must not be mistaken for the Windows default.

Observed in the supplied 11.1second recording (2fps contact sheet): idle in the opening frames; waveform visible by approximately1second; circular processing indicator around4.5seconds; text insertion and return to idle around5seconds; a second recording begins around6.5seconds, processes around9.5seconds, and returns to idle with additional text around10seconds. These are half-second sampled visual observations, **not measured key-release latency**: the keyboard event itself is not visible.

The icon remains the Notepad target while the waveform changes, and text appears in batches after processing. There is no visible word-by-word transcription in the supplied recording. **Live revisable text is an Utterly addition requested by the user**, not a feature established by this evidence. Utterly should show live text without expanding the always-on-top capsule into a large obstruction, and should collapse the recording waveform promptly on key release while retaining finalization state separately.

The user requested clipboard fallback when no text field is focused. That is an Utterly requirement; the supplied recording only demonstrates a focused Notepad insertion. The reference app source requests focused-app name/icon at recording start; it does not establish that the app icon alone proves a text control is editable. Utterly should separately inspect editability and confirm the same target before insertion.

## Settings structure and responsive behavior

Willow's default and minimum main window are **1250 × 900 logical pixels**, resizable, hidden custom titlebar. Its settings is a modal with maximum width940, height87vh capped660, corner radius24 and12px outer padding. Sidebar and content use flex layout; the content scrolls vertically. It is not designed as a mobile-width desktop layout. Copying its large minimum blindly would be poor on smaller displays; Utterly should preserve hierarchy while allowing narrower native windows.

Settings groups: radius8, subtle border/shadow, clipped children. Each row has left16/right24/top-bottom18px padding. Separators are1px, inset16px. Row title and description are separated8px; text flexes with min-width0, description max448px, action stays fixed at right. Title14px medium, description12px medium and muted. This creates the airy stacked grouping in the screenshots.

Light-theme source tokens: text`#3d3c40`, secondary`#8a888a`, tertiary`#b0afaf`, primary`#4443aa`, primary-dark`#373667`, subtle-violet`#e9e4fb`, background`#f3f3f5`, settings`#fafafa`, separator`#e9e8e8`. The user explicitly requires dark. The native adaptation should use the verified charcoal pill, dark surfaces/cards, readable white/gray text and restrained violet; do not merely invert screenshots.

Requested controls and observed source labels:

| Area | Reference behavior | Utterly implementation intent |
| --- | --- | --- |
| System | Interaction Sounds: starts/stops recording | Optional short native cues |
| System | Auto Duck Audio: description says automatically mutes system audio while recording | Reduce other audio and restore prior levels on every stop/error path |
| System | Mute All Notifications, including reminders | Suppress Utterly notifications; does not imply changing Windows global notifications |
| Intelligence | Context Awareness uses limited app text to spell names | Bounded editable-control context, opt-in, exclude password fields |
| Intelligence | Auto Dictionary learns proper nouns/names from screen | Conservative bounded local candidates, opt-in |
| Intelligence | Smart Text Insertion adjusts case, spacing and punctuation | Context-aware insertion with safe fallback |
| Appearance | Automatic Overlay Positioning | Active-monitor work-area placement |
| Appearance | Show Bubble When Not Recording | Optional tiny idle handle |
| Appearance | Hide Focused App Icon | Waveform-only pill option |
| Dictionary | Personal Terms and Personal Shortcuts | Existing local vocabulary retained and editable |

The reference also includes paid offline mode, Scribe commands, teams, billing and accounts. These are evidence of the reference app, not requested requirements to recreate all account/business infrastructure.

## Asset highlights

- Branding: `willow-logo-DVa1fDke.png`, `app-icon-UgZGqPUf.png`, extracted tray ICO. Retained as references; ship `icon.png` for Utterly.
- App illustrations/icons include ChatGPT, Claude, Cursor, Apple Mail, Notes, WhatsApp and others. Actual focused-app icons should come from the foreground application, not a fixed gallery.
- Interaction audio: `start-3-D_Hak66u.mp3`, `end-3-DRxDofCO.mp3`, `premium-reached-D2MkODeV.mp3`, `happy-EGVlu3hH.mp3`, `chime-Bp2MMWuD.mp3`, plus an embedded hands-free cue.
- Dictionary/onboarding cards, learning-center art, milestone art, collaboration art and auth illustrations are retained. They are not necessary for a lean working native settings interface.
- Spinner, waveform and many utility icons are vector/procedural source components rather than separate raster assets. Match their geometry in native drawing for crisp DPI scaling.
- The website image collection includes responsive variants and marketing/customer artwork. It is a reference library, not an endorsement to bundle every file.

## Public sources and limits

- [Official homepage](https://willowvoice.com/): product positioning and website font/assets.
- [Windows workflow](https://willowvoice.com/use-cases/windows): hold/speak/release workflow; claimed app compatibility and formatting.
- [Downloads](https://willowvoice.com/download): official Windows installer URL.
- [Pricing](https://willowvoice.com/pricing): public feature tiers; not a runtime benchmark.
- [Privacy policy](https://willowvoice.com/privacy-policy): public policy, not independent security validation.
- [Help center](https://help.willowvoice.com/en/), [Hotkey Settings](https://help.willowvoice.com/en/articles/10876257-hotkey-settings), [Microphone Settings](https://help.willowvoice.com/en/articles/10876169-microphone-settings), [Personal Dictionary and Shortcuts](https://help.willowvoice.com/en/articles/13183918-using-personal-dictionary-and-shortcuts): official feature documentation. The dictionary guide describes personal/team distinction, add/edit/delete terms, replacement shortcuts and cross-device sync. Utterly's local vocabulary does not imply cloud/team sync.
- [Public Windows release metadata](https://willow-electron-builds.s3.us-east-1.amazonaws.com/public/windows-x64/latest.yml): version and installer bytes.

No independent typing accuracy, end-to-end server latency, Willow RAM/CPU, real mixed-DPI movement, or accessibility behavior was benchmarked in this research. Asset inventory and geometry are exact packaged-file observations; video timing is sampled; product claims are attributed to official pages. Internet collection did not sign in, send messages, or upload the supplied recording.
