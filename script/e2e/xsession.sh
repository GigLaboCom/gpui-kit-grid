# An X11 session for windowed runs on Linux; sourced, not run.
#
#   source script/e2e/xsession.sh && start_x   # Xvfb, DISPLAY and the GPUI environment
#   ...                                         # windows, xdotool, xclip, import
#   stop_x
#
# No GPU: Mesa draws on the CPU. wgpu on Vulkan (lavapipe) leaves the window
# black under Xvfb, so Vulkan is hidden and wgpu takes its GL backend (EGL on
# X11, llvmpipe). Set VK_ICD_FILENAMES to try Vulkan anyway.

# The screen is larger than the blotter's 1400x900 window.
X_SCREEN=${X_SCREEN:-1600x1000x24}
BLOTTER_BIN=${BLOTTER_BIN:-/target/release/examples/blotter}
PORTFOLIO_BIN=${PORTFOLIO_BIN:-/target/release/examples/portfolio}
DOCK_BIN=${DOCK_BIN:-/target/release/examples/dock_layout}
ORDERS_BIN=${ORDERS_BIN:-/target/release/examples/orders}

start_x() {
  export VK_ICD_FILENAMES=${VK_ICD_FILENAMES:-/nonexistent.json}
  export WAYLAND_DISPLAY= XDG_RUNTIME_DIR=/tmp/xdg
  export RUST_LOG=${RUST_LOG:-warn,wgpu_core=error,wgpu_hal=error,naga=error}
  mkdir -p /tmp/xdg && chmod 700 /tmp/xdg
  local display=":$((100 + RANDOM % 800))"
  Xvfb "$display" -screen 0 "$X_SCREEN" -nolisten tcp >/tmp/xvfb.log 2>&1 &
  XVFB_PID=$!
  export DISPLAY=$display
  for _ in $(seq 1 50); do
    xdpyinfo >/dev/null 2>&1 && return 0
    sleep 0.1
  done
  echo "Xvfb did not start on $display" >&2
  cat /tmp/xvfb.log >&2
  return 1
}

stop_x() {
  if [[ -n ${XVFB_PID:-} ]]; then
    kill "$XVFB_PID" 2>/dev/null || true
    wait "$XVFB_PID" 2>/dev/null || true
  fi
}
