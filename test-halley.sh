#!/usr/bin/env bash
set -e

# Halley Test Launcher
# Run this script to test the newly built Halley compositor with Noctalia.

BIN="/home/ghostboy/.local/bin/halley"
if [ ! -f "$BIN" ]; then
    BIN="/home/ghostboy/halley/target/release/halley"
fi

echo "=== Halley Test Launcher ==="
echo "Binary: $BIN ($($BIN --version 2>/dev/null | tail -n1))"
echo "Current Environment: WAYLAND_DISPLAY=${WAYLAND_DISPLAY:-none}"

case "$1" in
    --noctalia)
        echo "Starting Halley nested with Noctalia..."
        # Launch nested Halley in background and run noctalia inside it
        $BIN --winit &
        HALLEY_PID=$!
        sleep 1.5
        # Find the nested socket
        NESTED_SOCKET=$(ls -t /run/user/$(id -u)/wayland-* 2>/dev/null | head -n1 | xargs basename)
        echo "Spawning Noctalia on $NESTED_SOCKET..."
        WAYLAND_DISPLAY="$NESTED_SOCKET" noctalia &
        NOCTALIA_PID=$!
        echo "Press Ctrl+C to stop the test session."
        trap "kill $NOCTALIA_PID $HALLEY_PID 2>/dev/null || true" EXIT
        wait $HALLEY_PID
        ;;
    --session)
        echo "Starting full TTY session (run this from a TTY console, not under Hyprland)..."
        exec $BIN --session
        ;;
    *)
        echo "Starting Halley nested under your current desktop..."
        echo "Tip: Pass '--noctalia' to also automatically launch Noctalia inside the nested window:"
        echo "     ./test-halley.sh --noctalia"
        echo ""
        exec $BIN --winit
        ;;
esac
