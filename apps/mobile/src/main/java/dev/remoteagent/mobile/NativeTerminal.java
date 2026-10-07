package dev.remoteagent.mobile;

import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.view.KeyEvent;
import android.view.MotionEvent;
import android.view.inputmethod.InputMethodManager;
import com.termux.terminal.TerminalSession;
import com.termux.terminal.TerminalSessionClient;
import com.termux.view.TerminalView;
import com.termux.view.TerminalViewClient;

/** Platform input, selection and clipboard belong to the native terminal view. */
public final class NativeTerminal implements TerminalSessionClient, TerminalViewClient {
    public final TerminalView view;
    public final TerminalSession session;
    private long sequence;
    public boolean controlArmed;
    public boolean altArmed;
    public Runnable onModifiersReleased;
    public NativeTerminal(Context context, TerminalSession.Transport transport, int background, int foreground, int cursor, int fontSize) {
        view = new TerminalView(context, null);
        view.setTerminalViewClient(this);
        view.setBackgroundColor(background);
        view.setId(R.id.native_terminal);
        view.setTextSize((int) (fontSize * context.getResources().getDisplayMetrics().scaledDensity));
        session = new TerminalSession(transport, this);
        session.getEmulator().mColors.mCurrentColors[com.termux.terminal.TextStyle.COLOR_INDEX_BACKGROUND] = background;
        session.getEmulator().mColors.mCurrentColors[com.termux.terminal.TextStyle.COLOR_INDEX_FOREGROUND] = foreground;
        session.getEmulator().mColors.mCurrentColors[com.termux.terminal.TextStyle.COLOR_INDEX_CURSOR] = cursor;
        view.attachSession(session);
        view.setFocusableInTouchMode(true);
    }
    public boolean feed(long next, byte[] bytes, int columns, int rows) {
        if (next <= sequence) return false;
        if (columns > 0) session.restoreSize(columns, rows);
        session.feed(bytes);
        sequence = next;
        if (columns > 0) view.updateSize();
        return true;
    }
    public void onTextChanged(TerminalSession session) { view.onScreenUpdated(); }
    public void onTitleChanged(TerminalSession session) {}
    public void onSessionFinished(TerminalSession session) {}
    public void onCopyTextToClipboard(TerminalSession session, String text) {
        view.getContext().getSystemService(ClipboardManager.class).setPrimaryClip(ClipData.newPlainText("", text));
    }
    public void onPasteTextFromClipboard(TerminalSession session) {
        ClipboardManager clipboard = view.getContext().getSystemService(ClipboardManager.class);
        if (clipboard.hasPrimaryClip()) this.session.getEmulator().paste(clipboard.getPrimaryClip().getItemAt(0).coerceToText(view.getContext()).toString());
    }
    public void onBell(TerminalSession session) { view.performHapticFeedback(android.view.HapticFeedbackConstants.KEYBOARD_TAP); }
    public void onColorsChanged(TerminalSession session) {
        var emulator = session.getEmulator();
        // TerminalEmulator.reset also notifies while its constructor is running.
        if (emulator != null) view.setBackgroundColor(emulator.mColors.mCurrentColors[com.termux.terminal.TextStyle.COLOR_INDEX_BACKGROUND]);
        view.invalidate();
    }
    public void onTerminalCursorStateChange(boolean state) { view.invalidate(); }
    public void setTerminalShellPid(TerminalSession session, int pid) {}
    public Integer getTerminalCursorStyle() { return null; }
    public float onScale(float scale) { return 1; }
    public void onSingleTapUp(MotionEvent event) {
        view.requestFocus();
        view.getContext().getSystemService(InputMethodManager.class).showSoftInput(view, InputMethodManager.SHOW_IMPLICIT);
    }
    public boolean shouldBackButtonBeMappedToEscape() { return false; }
    public boolean shouldEnforceCharBasedInput() { return true; }
    public boolean shouldUseCtrlSpaceWorkaround() { return false; }
    public boolean isTerminalViewSelected() { return true; }
    public void copyModeChanged(boolean copying) {}
    public boolean onKeyDown(int keyCode, KeyEvent event, TerminalSession session) { return false; }
    public boolean onKeyUp(int keyCode, KeyEvent event) { return false; }
    public boolean onLongPress(MotionEvent event) { return false; }
    public boolean readControlKey() { return controlArmed; }
    public boolean readAltKey() { return altArmed; }
    public boolean readShiftKey() { return false; }
    public boolean readFnKey() { return false; }
    public boolean onCodePoint(int codePoint, boolean control, TerminalSession session) {
        releaseModifiers();
        return false;
    }
    // One-shot modifiers from the extra keys row apply to the next key.
    public void releaseModifiers() {
        if (!controlArmed && !altArmed) return;
        controlArmed = false;
        altArmed = false;
        if (onModifiersReleased != null) onModifiersReleased.run();
    }
    public void onEmulatorSet() {}
    // Terminal input/output may contain credentials; do not forward emulator logs.
    public void logError(String tag, String message) {}
    public void logWarn(String tag, String message) {}
    public void logInfo(String tag, String message) {}
    public void logDebug(String tag, String message) {}
    public void logVerbose(String tag, String message) {}
    public void logStackTraceWithMessage(String tag, String message, Exception error) {}
    public void logStackTrace(String tag, Exception error) {}
}
