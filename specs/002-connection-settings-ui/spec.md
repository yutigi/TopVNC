# Connection and in-session settings UI

## Scope

Add a native connection landing screen with address, port, VNC password, window, and speed controls. Show in-session settings over the viewer for render mode, image scaling, and update rate. Keep the current Raw RFB and TCP session behavior.

## Acceptance criteria

- Starting without arguments opens a connection form, validates host and port, and reports connection errors in the form.
- VNC passwords are masked, never logged, and supplied only to standard VNC password authentication. The UI explains that username authentication is not implemented and that RFB traffic is unencrypted.
- Initial window mode and size can be chosen before connection. The connected window can be resized by dragging.
- Settings are reachable while streaming by button or F8; keyboard and pointer events used by the settings UI are not forwarded to the server.
- Update rate and image scaling choices take effect during a session. A user can disconnect back to the landing screen.
- Invalid dimensions, port, and address are rejected before connecting.

## Stalled login image recovery

- Bound address lookup/TCP connection and the RFB handshake to ten seconds each so a silent server cannot leave the form connecting indefinitely.
- After five seconds without framebuffer stream data, send one non-incremental full-screen request. Preserve parser progress across partial headers and pixel payloads; never restart a partially consumed message.
- If a second five-second read wait expires within the same update, end the session and show an actionable error on the connection form. Close the connection on every viewer exit, including errors.
- Verify recovery with a TCP server that stops answering incremental requests, responds to the full refresh, and then resumes incremental updates. Verify timeout handling and partial-message integrity separately.
- Live acceptance: connect to a locked Mac, unlock it locally, and confirm the viewer displays the desktop without restarting. This remains pending access to a live Mac.
