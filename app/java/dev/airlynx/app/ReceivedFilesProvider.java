package dev.airlynx.app;

import android.content.ContentProvider;
import android.content.ContentValues;
import android.database.Cursor;
import android.database.MatrixCursor;
import android.net.Uri;
import android.os.Environment;
import android.os.ParcelFileDescriptor;
import android.provider.OpenableColumns;
import android.webkit.MimeTypeMap;

import java.io.File;
import java.io.FileNotFoundException;
import java.io.IOException;
import java.util.Locale;

/**
 * Lets the app that opens a received file read it. A file:// URI is refused for apps targeting Android 7+, and
 * androidx's FileProvider is not available because the APK is not built with Gradle, so this serves the
 * received folders read-only. The URI path is the absolute file path, which also gives the other app the real
 * file name. Not exported: only apps we hand a URI to with FLAG_GRANT_READ_URI_PERMISSION can read it.
 */
public class ReceivedFilesProvider extends ContentProvider {
    public static final String AUTHORITY = "dev.airlynx.app.files";

    public static Uri uriFor(File file) {
        return new Uri.Builder().scheme("content").authority(AUTHORITY).path(file.getAbsolutePath()).build();
    }

    @Override
    public boolean onCreate() {
        return true;
    }

    @Override
    public String getType(Uri uri) {
        return mimeType(uri.getLastPathSegment());
    }

    @Override
    public Cursor query(Uri uri, String[] projection, String selection, String[] selectionArgs, String sortOrder) {
        File file;
        try {
            file = resolve(uri);
        } catch (FileNotFoundException e) {
            return null;
        }
        String[] columns = projection != null ? projection : new String[] {OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE};
        Object[] row = new Object[columns.length];
        for (int i = 0; i < columns.length; i++) {
            if (OpenableColumns.DISPLAY_NAME.equals(columns[i])) {
                row[i] = file.getName();
            } else if (OpenableColumns.SIZE.equals(columns[i])) {
                row[i] = file.length();
            }
        }
        MatrixCursor cursor = new MatrixCursor(columns, 1);
        cursor.addRow(row);
        return cursor;
    }

    @Override
    public ParcelFileDescriptor openFile(Uri uri, String mode) throws FileNotFoundException {
        if (!"r".equals(mode)) {
            throw new FileNotFoundException("read only");
        }
        return ParcelFileDescriptor.open(resolve(uri), ParcelFileDescriptor.MODE_READ_ONLY);
    }

    @Override
    public Uri insert(Uri uri, ContentValues values) {
        throw new UnsupportedOperationException("read only");
    }

    @Override
    public int update(Uri uri, ContentValues values, String selection, String[] selectionArgs) {
        throw new UnsupportedOperationException("read only");
    }

    @Override
    public int delete(Uri uri, String selection, String[] selectionArgs) {
        throw new UnsupportedOperationException("read only");
    }

    /** The file behind `uri`, if it is a regular file inside one of the receive folders. */
    private File resolve(Uri uri) throws FileNotFoundException {
        String path = uri.getPath();
        if (path == null) {
            throw new FileNotFoundException(String.valueOf(uri));
        }
        try {
            File file = new File(path).getCanonicalFile();
            for (File root : roots()) {
                if (root != null && file.getPath().startsWith(root.getCanonicalPath() + File.separator) && file.isFile()) {
                    return file;
                }
            }
        } catch (IOException ignored) {
        }
        throw new FileNotFoundException(String.valueOf(uri));
    }

    /** Where received files go: see default_receive_dir in android.rs. */
    private File[] roots() {
        return new File[] {
            new File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS), "AirLynx"),
            new File(getContext().getExternalFilesDir(null), "received"),
            // Last resort of default_receive_dir. Not the whole files dir: config/ holds the device id.
            new File(getContext().getFilesDir(), "received"),
        };
    }

    static String mimeType(String name) {
        if (name != null) {
            int dot = name.lastIndexOf('.');
            if (dot >= 0 && dot < name.length() - 1) {
                String type = MimeTypeMap.getSingleton().getMimeTypeFromExtension(name.substring(dot + 1).toLowerCase(Locale.ROOT));
                if (type != null) {
                    return type;
                }
            }
        }
        return "application/octet-stream";
    }
}
