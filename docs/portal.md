# Screen sharing and the desktop portal

Halley's desktop portal is the safe bridge used by OBS, browsers, and chat
applications when they ask to capture the desktop. The application does not
choose a compositor surface directly. Halley opens its own modal chooser and
returns only the source the user confirms.

## Choosing a source

When an application supports both source types, the chooser first shows a
bottom bar:

- **Monitor** enters monitor-picking mode.
- **Window** enters window-picking mode.

An unsupported entry remains visible but disabled, so it is clear why it
cannot be selected. Use the pointer or Left/Right and Enter. Escape returns
from a picking mode to the bottom bar; pressing Escape on the bar cancels the
request.

Monitor and window picking dim everything outside the candidate. This is only
chooser chrome and is never included in the resulting stream.

## OBS

Add a **Screen Capture (PipeWire)** source in OBS. Halley's Monitor/Window bar
should appear, followed by the appropriate click-to-pick mode. Once confirmed,
OBS receives a live PipeWire stream. Closing the OBS source closes the portal
session and removes its PipeWire node.

The portal supports hidden, embedded, and metadata cursor modes. The requesting
application chooses the mode; no Halley configuration key is required.

Halley currently negotiates mapped PipeWire memory for all streams. Its
DMA-BUF implementation remains isolated in the codebase but is not advertised
until explicit frame-completion synchronization is available; exposing
unsynchronized DMA-BUFs can make consumers display stale or black frames.

## Session routing

`XDG_CURRENT_DESKTOP` must include `Halley`, and the installed
`halley-portals.conf` routes the ScreenCast and Screenshot interfaces to
`xdg-desktop-portal-halley`. Distribution packages install the matching D-Bus
and systemd user-service files from `packaging/`.
