package dev.airlynx.app;

import android.app.NativeActivity;
import android.content.ActivityNotFoundException;
import android.content.ClipData;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.provider.OpenableColumns;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;

/**
 * The app's activity: NativeActivity plus the "Share to AirLynx" target. Android hands shared files over
 * as content:// URIs that only this activity may read, so they are copied into the app's cache and the
 * native side (polling over JNI, see android.rs) picks up plain file paths. Copying runs on a thread,
 * because a shared video can be gigabytes.
 *
 * launchMode is singleTask (see Cargo.toml), so a share while the app is running arrives in
 * onNewIntent instead of starting a second native activity.
 */
public class MainActivity extends NativeActivity {
    private static final String SHARE_DIR = "share";

    /** Copied files not yet picked up by the native side, and the work still running. Guarded by this. */
    private final ArrayList<String> ready = new ArrayList<>();
    private int preparing;
    private String error;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        // Copies from an earlier run are orphans: the native side keeps its file list in memory only.
        deleteTree(new File(getCacheDir(), SHARE_DIR));
        super.onCreate(savedInstanceState);
        // After a recreate the same intent is still there, and its files must not be added twice.
        if (savedInstanceState == null) {
            handleShare(getIntent());
        }
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        handleShare(intent);
    }

    // ---- called from native code ------------------------------------------------------------

    /** Paths of the files copied since the last call. */
    public synchronized String[] takeSharedFiles() {
        String[] paths = ready.toArray(new String[0]);
        ready.clear();
        return paths;
    }

    /** How many shared items are still being copied. */
    public synchronized int sharedPreparing() {
        return preparing;
    }

    /** A message for the last copy that failed, once; null if none did. */
    public synchronized String takeShareError() {
        String message = error;
        error = null;
        return message;
    }

    /** Deletes a copy made by this class once it is sent or removed. Other paths are left alone. */
    public void discardShared(String path) {
        File file = new File(path);
        try {
            String root = new File(getCacheDir(), SHARE_DIR).getCanonicalPath() + File.separator;
            if (file.getCanonicalPath().startsWith(root)) {
                file.delete();
                File parent = file.getParentFile();
                if (parent != null) {
                    parent.delete(); // only succeeds when the batch folder is empty
                }
            }
        } catch (IOException ignored) {
        }
    }

    /** Opens a received file in whichever app handles its type. Returns null, or why it could not be opened. */
    public String openFile(String path) {
        File file = new File(path);
        if (!file.isFile()) {
            return "The file no longer exists";
        }
        Intent intent = new Intent(Intent.ACTION_VIEW);
        intent.setDataAndType(ReceivedFilesProvider.uriFor(file), ReceivedFilesProvider.mimeType(file.getName()));
        intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        try {
            startActivity(intent);
            return null;
        } catch (ActivityNotFoundException e) {
            return "No app can open this type of file";
        } catch (RuntimeException e) {
            return String.valueOf(e.getMessage());
        }
    }

    // ---- share intents ----------------------------------------------------------------------

    private void handleShare(Intent intent) {
        if (intent == null) {
            return;
        }
        String action = intent.getAction();
        if (!Intent.ACTION_SEND.equals(action) && !Intent.ACTION_SEND_MULTIPLE.equals(action)) {
            return;
        }
        final ArrayList<Uri> uris = streams(intent);
        final String text = uris.isEmpty() ? intent.getStringExtra(Intent.EXTRA_TEXT) : null;
        final String title = firstNonEmpty(intent.getStringExtra(Intent.EXTRA_SUBJECT), intent.getStringExtra(Intent.EXTRA_TITLE));
        if (uris.isEmpty() && (text == null || text.isEmpty())) {
            return;
        }
        synchronized (this) {
            preparing += Math.max(uris.size(), 1);
        }
        new Thread(() -> {
            File dir = new File(new File(getCacheDir(), SHARE_DIR), Long.toString(System.nanoTime()));
            if (!uris.isEmpty()) {
                for (Uri uri : uris) {
                    copyOne(dir, uri);
                }
            } else {
                saveText(dir, title, text);
            }
        }, "airlynx-share").start();
    }

    @SuppressWarnings("deprecation")
    private static ArrayList<Uri> streams(Intent intent) {
        ArrayList<Uri> uris = new ArrayList<>();
        if (Intent.ACTION_SEND_MULTIPLE.equals(intent.getAction())) {
            ArrayList<Uri> list = Build.VERSION.SDK_INT >= 33
                    ? intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM, Uri.class)
                    : intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM);
            if (list != null) {
                uris.addAll(list);
            }
        } else {
            Uri one = Build.VERSION.SDK_INT >= 33
                    ? intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri.class)
                    : intent.getParcelableExtra(Intent.EXTRA_STREAM);
            if (one != null) {
                uris.add(one);
            }
        }
        // Some apps only fill the ClipData.
        ClipData clip = intent.getClipData();
        if (uris.isEmpty() && clip != null) {
            for (int i = 0; i < clip.getItemCount(); i++) {
                Uri uri = clip.getItemAt(i).getUri();
                if (uri != null) {
                    uris.add(uri);
                }
            }
        }
        return uris;
    }

    private void copyOne(File dir, Uri uri) {
        File target = null;
        try (InputStream in = getContentResolver().openInputStream(uri)) {
            if (in == null) {
                throw new IOException("cannot open " + uri);
            }
            target = uniqueFile(dir, displayName(uri));
            copy(in, target);
            itemDone(target.getPath(), null);
        } catch (IOException | RuntimeException e) {
            if (target != null) {
                target.delete();
            }
            itemDone(null, "Could not read a shared file: " + e.getMessage());
        }
    }

    private void saveText(File dir, String title, String text) {
        File target = null;
        try {
            target = uniqueFile(dir, sanitize(title == null ? "Shared text" : title) + ".txt");
            try (OutputStream out = new FileOutputStream(target)) {
                out.write(text.getBytes(StandardCharsets.UTF_8));
            }
            itemDone(target.getPath(), null);
        } catch (IOException e) {
            if (target != null) {
                target.delete();
            }
            itemDone(null, "Could not save the shared text: " + e.getMessage());
        }
    }

    /** One item is done: publish its path, or the error, and count it off. */
    private synchronized void itemDone(String path, String failure) {
        preparing = Math.max(preparing - 1, 0);
        if (path != null) {
            ready.add(path);
        }
        if (failure != null) {
            error = failure;
        }
    }

    // ---- helpers ----------------------------------------------------------------------------

    private String displayName(Uri uri) {
        String name = null;
        if ("content".equals(uri.getScheme())) {
            try (Cursor cursor = getContentResolver().query(uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
                if (cursor != null && cursor.moveToFirst()) {
                    name = cursor.getString(0);
                }
            } catch (RuntimeException ignored) {
            }
        }
        if (name == null || name.isEmpty()) {
            name = uri.getLastPathSegment();
        }
        return sanitize(name);
    }

    /** A name the receiver will accept: no separators or control characters, never empty. */
    private static String sanitize(String name) {
        String clean = name == null ? "" : name.replaceAll("[/\\\\\\p{Cntrl}]", "_").trim();
        if (clean.length() > 200) {
            clean = clean.substring(0, 200);
        }
        return clean.isEmpty() || clean.equals(".") || clean.equals("..") ? "shared-file" : clean;
    }

    /** `name`, or `name (1)`, `name (2)` and so on when the batch already has one. */
    private static File uniqueFile(File dir, String name) throws IOException {
        if (!dir.isDirectory() && !dir.mkdirs()) {
            throw new IOException("cannot create " + dir);
        }
        File file = new File(dir, name);
        int dot = name.lastIndexOf('.');
        String stem = dot > 0 ? name.substring(0, dot) : name;
        String ext = dot > 0 ? name.substring(dot) : "";
        for (int i = 1; file.exists(); i++) {
            file = new File(dir, stem + " (" + i + ")" + ext);
        }
        return file;
    }

    private static void copy(InputStream in, File target) throws IOException {
        try (OutputStream out = new FileOutputStream(target)) {
            byte[] buffer = new byte[64 * 1024];
            int n;
            while ((n = in.read(buffer)) > 0) {
                out.write(buffer, 0, n);
            }
        }
    }

    private static String firstNonEmpty(String a, String b) {
        return a != null && !a.trim().isEmpty() ? a : b;
    }

    private static void deleteTree(File file) {
        File[] children = file.listFiles();
        if (children != null) {
            for (File child : children) {
                deleteTree(child);
            }
        }
        file.delete();
    }
}
