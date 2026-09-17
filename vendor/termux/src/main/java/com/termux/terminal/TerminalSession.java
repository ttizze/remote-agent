package com.termux.terminal;

import java.util.Arrays;

/** Bex byte-stream session. The authenticated Host owns the PTY. */
public final class TerminalSession extends TerminalOutput {
    public interface Transport {
        void write(byte[] data);
        void resize(int columns, int rows);
    }
    private final Transport transport;
    private final TerminalSessionClient client;
    private final TerminalEmulator emulator;
    private boolean feeding;
    public TerminalSession(Transport transport, TerminalSessionClient client) {
        this.transport = transport;
        this.client = client;
        emulator = new TerminalEmulator(this, 80, 24, 8, 16, 10000, client);
    }
    public TerminalEmulator getEmulator() { return emulator; }
    public void updateSize(int columns, int rows, int width, int height) {
        columns = Math.max(2, Math.min(500, columns));
        rows = Math.max(1, Math.min(250, rows));
        emulator.resize(columns, rows, width, height);
        transport.resize(columns, rows);
    }
    public void restoreSize(int columns, int rows) { emulator.resize(columns, rows, 8, 16); }
    public void feed(byte[] bytes) { feeding = true; try { emulator.append(bytes, bytes.length); } finally { feeding = false; } client.onTextChanged(this); }
    public void writeCodePoint(boolean alt, int codePoint) {
        write((alt ? "\u001b" : "") + new String(Character.toChars(codePoint)));
    }
    @Override public void write(byte[] bytes, int offset, int count) {
        if (feeding) return; // The Host alone replies to terminal queries.
        transport.write(Arrays.copyOfRange(bytes, offset, offset + count));
    }
    @Override public void titleChanged(String oldTitle, String newTitle) { client.onTitleChanged(this); }
    @Override public void onCopyTextToClipboard(String text) { client.onCopyTextToClipboard(this, text); }
    @Override public void onPasteTextFromClipboard() { client.onPasteTextFromClipboard(this); }
    @Override public void onBell() { client.onBell(this); }
    @Override public void onColorsChanged() { client.onColorsChanged(this); }
}
