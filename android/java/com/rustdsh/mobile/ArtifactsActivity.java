package com.rustdsh.mobile;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.ContentValues;
import android.content.DialogInterface;
import android.content.pm.PackageManager;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.Environment;
import android.provider.MediaStore;
import android.view.View;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.TextView;
import android.widget.Toast;

import org.json.JSONArray;
import org.json.JSONObject;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.text.SimpleDateFormat;
import java.util.ArrayList;
import java.util.Date;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;

/**
 * 收纳箱（v2.5）：把智能体产物按「工作台产物 / 会话记录」分类列出，
 * 列表数据来自 Rust 核心（NativeBridge.dshListArtifacts 返回
 * {"files":[{"rel","size","mtime"}]}，rel 相对 filesDir），
 * 每个文件可以「提取」（复制到手机 Download/dsh-export/，文件管理器可见）
 * 或「删除」（NativeBridge.dshDeletePath）。数据都在应用私有目录里，
 * 普通文件管理器看不到，这里提供唯一入口。
 */
public class ArtifactsActivity extends Activity {

    private static final int REQ_STORAGE = 41;

    private LinearLayout container;
    private String pendingRel;

    /** 一条产物记录（来自 Rust 的 JSON）。 */
    private static class ArtEntry {
        String rel;    // 相对 filesDir 的路径，如 workspace/a.txt 或 sessions/ws/sess/x
        long size;
        long mtime;    // 毫秒时间戳
    }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_artifacts);
        container = findViewById(R.id.artifacts_container);
    }

    @Override
    protected void onResume() {
        super.onResume();
        refresh();
    }

    // ---------- 列表构建 ----------

    private void refresh() {
        container.removeAllViews();
        String json = NativeBridge.dshListArtifacts(getFilesDir().getAbsolutePath());
        List<ArtEntry> all = parseJson(json);

        // 分类 1：工作台产物（workspace/）
        List<ArtEntry> ws = new ArrayList<>();
        // 分类 2：会话记录（sessions/<ws>/<sess>/），按会话目录分组
        Map<String, List<ArtEntry>> sessions = new LinkedHashMap<>();
        for (ArtEntry e : all) {
            if (e.rel.startsWith("workspace/")) {
                ws.add(e);
            } else if (e.rel.startsWith("sessions/")) {
                int idx = e.rel.indexOf('/', "sessions/".length());
                if (idx <= 0) continue;
                String dir = e.rel.substring(0, idx); // sessions/<ws>/<sess>
                List<ArtEntry> list = sessions.get(dir);
                if (list == null) { list = new ArrayList<>(); sessions.put(dir, list); }
                list.add(e);
            }
        }

        int count = 0;

        addCategoryHeader(getString(R.string.cat_workspace), ws);
        if (ws.isEmpty()) {
            addEmpty(getString(R.string.artifacts_empty));
        } else {
            for (ArtEntry e : ws) {
                addRow(e);
                count++;
            }
        }

        addCategoryHeader(getString(R.string.cat_sessions), null);
        if (sessions.isEmpty()) {
            addEmpty(getString(R.string.artifacts_empty));
        } else {
            for (Map.Entry<String, List<ArtEntry>> me : sessions.entrySet()) {
                addSessionHeader(me.getKey(), me.getValue());
                for (ArtEntry e : me.getValue()) {
                    addRow(e);
                    count++;
                }
            }
        }

        TextView stat = findViewById(R.id.artifacts_stat);
        stat.setText(getString(R.string.artifacts_stat, count));
    }

    private static List<ArtEntry> parseJson(String json) {
        List<ArtEntry> out = new ArrayList<>();
        if (json == null || json.isEmpty()) return out;
        try {
            JSONObject obj = new JSONObject(json);
            JSONArray files = obj.optJSONArray("files");
            if (files != null) {
                for (int i = 0; i < files.length(); i++) {
                    JSONObject o = files.optJSONObject(i);
                    if (o == null) continue;
                    ArtEntry e = new ArtEntry();
                    e.rel = o.optString("rel");
                    e.size = o.optLong("size");
                    e.mtime = o.optLong("mtime");
                    if (!e.rel.isEmpty()) out.add(e);
                }
            }
        } catch (Exception ignored) {
            // 异常按空列表处理
        }
        return out;
    }

    private void addCategoryHeader(String text, final List<ArtEntry> extractAllFiles) {
        TextView tv = new TextView(this);
        tv.setText(text);
        tv.setTextSize(15);
        tv.setTextColor(getColor(R.color.text_primary));
        tv.setTypeface(null, android.graphics.Typeface.BOLD);
        LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT);
        lp.topMargin = dp(6);
        lp.bottomMargin = dp(8);
        tv.setLayoutParams(lp);
        container.addView(tv);
        if (extractAllFiles != null && !extractAllFiles.isEmpty()) {
            Button all = new Button(new android.view.ContextThemeWrapper(this, R.style.DshButtonPrimary));
            all.setText(R.string.btn_extract_all);
            LinearLayout.LayoutParams blp = new LinearLayout.LayoutParams(
                    LinearLayout.LayoutParams.WRAP_CONTENT, LinearLayout.LayoutParams.WRAP_CONTENT);
            blp.bottomMargin = dp(8);
            all.setLayoutParams(blp);
            all.setOnClickListener(new View.OnClickListener() {
                @Override public void onClick(View v) {
                    extractAll(extractAllFiles);
                }
            });
            container.addView(all);
        }
    }

    private void addSessionHeader(final String dirRel, List<ArtEntry> files) {
        String sessName = dirRel.substring(dirRel.lastIndexOf('/') + 1);
        long total = 0;
        long mtime = 0;
        for (ArtEntry e : files) {
            total += e.size;
            if (e.mtime > mtime) mtime = e.mtime;
        }

        LinearLayout row = new LinearLayout(this);
        row.setOrientation(LinearLayout.HORIZONTAL);
        row.setPadding(dp(12), dp(10), dp(12), dp(10));
        row.setBackgroundResource(R.drawable.bg_card);
        LinearLayout.LayoutParams rlp = new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT);
        rlp.bottomMargin = dp(8);
        row.setLayoutParams(rlp);

        LinearLayout left = new LinearLayout(this);
        left.setOrientation(LinearLayout.VERTICAL);
        LinearLayout.LayoutParams llp = new LinearLayout.LayoutParams(0,
                LinearLayout.LayoutParams.WRAP_CONTENT, 1f);
        left.setLayoutParams(llp);

        TextView name = new TextView(this);
        name.setText("会话 " + shortId(sessName));
        name.setTextSize(13);
        name.setTextColor(getColor(R.color.text_primary));
        name.setTypeface(null, android.graphics.Typeface.BOLD);
        left.addView(name);

        TextView meta = new TextView(this);
        meta.setText(fmtDate(mtime) + " · " + files.size() + " 个文件 · " + fmtSize(total));
        meta.setTextSize(11);
        meta.setTextColor(getColor(R.color.text_secondary));
        left.addView(meta);

        row.addView(left);

        row.addView(makeButton(R.string.btn_extract, R.style.DshButtonPrimary, new View.OnClickListener() {
            @Override public void onClick(View v) { extractAll(files); }
        }));
        row.addView(makeButton(R.string.btn_delete, R.style.DshButtonDanger, new View.OnClickListener() {
            @Override public void onClick(View v) { confirmDelete(dirRel, "会话 " + shortId(sessName)); }
        }));
        container.addView(row);
    }

    private void addRow(final ArtEntry e) {
        final String name = e.rel.substring(e.rel.lastIndexOf('/') + 1);

        LinearLayout row = new LinearLayout(this);
        row.setOrientation(LinearLayout.HORIZONTAL);
        row.setPadding(dp(12), dp(10), dp(12), dp(10));
        row.setBackgroundResource(R.drawable.bg_card);
        LinearLayout.LayoutParams rlp = new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT);
        rlp.bottomMargin = dp(8);
        row.setLayoutParams(rlp);

        LinearLayout left = new LinearLayout(this);
        left.setOrientation(LinearLayout.VERTICAL);
        LinearLayout.LayoutParams llp = new LinearLayout.LayoutParams(0,
                LinearLayout.LayoutParams.WRAP_CONTENT, 1f);
        left.setLayoutParams(llp);

        TextView nameView = new TextView(this);
        nameView.setText(name);
        nameView.setTextSize(13);
        nameView.setTextColor(getColor(R.color.text_primary));
        left.addView(nameView);

        TextView meta = new TextView(this);
        meta.setText(fmtDate(e.mtime) + " · " + fmtSize(e.size));
        meta.setTextSize(11);
        meta.setTextColor(getColor(R.color.text_secondary));
        left.addView(meta);

        row.addView(left);
        row.addView(makeButton(R.string.btn_extract, R.style.DshButtonPrimary, new View.OnClickListener() {
            @Override public void onClick(View v) { extractOne(e.rel); }
        }));
        row.addView(makeButton(R.string.btn_delete, R.style.DshButtonDanger, new View.OnClickListener() {
            @Override public void onClick(View v) { confirmDelete(e.rel, name); }
        }));
        container.addView(row);
    }

    private Button makeButton(int textRes, int styleRes, View.OnClickListener listener) {
        Button b = new Button(new android.view.ContextThemeWrapper(this, styleRes));
        b.setText(textRes);
        b.setTextSize(12);
        b.setMinWidth(0);
        b.setMinHeight(0);
        b.setPadding(dp(12), dp(4), dp(12), dp(4));
        LinearLayout.LayoutParams blp = new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.WRAP_CONTENT, LinearLayout.LayoutParams.WRAP_CONTENT);
        blp.leftMargin = dp(8);
        b.setLayoutParams(blp);
        b.setOnClickListener(listener);
        return b;
    }

    private void addEmpty(String text) {
        TextView tv = new TextView(this);
        tv.setText(text);
        tv.setTextSize(13);
        tv.setTextColor(getColor(R.color.text_secondary));
        tv.setPadding(dp(14), 0, 0, dp(8));
        container.addView(tv);
    }

    // ---------- 提取 ----------

    private void extractOne(String rel) {
        if (!hasStoragePermission()) {
            pendingRel = rel;
            if (Build.VERSION.SDK_INT >= 23) {
                requestPermissions(new String[]{"android.permission.WRITE_EXTERNAL_STORAGE"}, REQ_STORAGE);
            }
            return;
        }
        doExtract(new File(getFilesDir(), rel), rel);
    }

    private void extractAll(List<ArtEntry> files) {
        if (!hasStoragePermission()) {
            Toast.makeText(this, R.string.perm_storage, Toast.LENGTH_SHORT).show();
            if (Build.VERSION.SDK_INT >= 23) {
                requestPermissions(new String[]{"android.permission.WRITE_EXTERNAL_STORAGE"}, REQ_STORAGE);
            }
            return;
        }
        int ok = 0;
        for (ArtEntry e : files) {
            if (doExtract(new File(getFilesDir(), e.rel), e.rel)) ok++;
        }
        Toast.makeText(this, getString(R.string.extract_all_ok, ok, files.size()),
                Toast.LENGTH_LONG).show();
    }

    private boolean hasStoragePermission() {
        return Build.VERSION.SDK_INT < 23
                || checkSelfPermission("android.permission.WRITE_EXTERNAL_STORAGE")
                == PackageManager.PERMISSION_GRANTED;
    }

    private boolean doExtract(File src, String rel) {
        // 首选直接写 Download/dsh-export（targetSdk 28 走 legacy 存储模型）
        File out = new File(Environment.getExternalStoragePublicDirectory(
                Environment.DIRECTORY_DOWNLOADS), "dsh-export/" + rel);
        try {
            File parent = out.getParentFile();
            if (parent != null && !parent.exists()) parent.mkdirs();
            copyStream(new FileInputStream(src), new FileOutputStream(out));
            Toast.makeText(this, getString(R.string.extract_ok, rel), Toast.LENGTH_SHORT).show();
            return true;
        } catch (Exception e1) {
            // 兜底：MediaStore Downloads 集合（Android 10+ 的 scoped storage 也兼容）
            if (Build.VERSION.SDK_INT >= 29) {
                try {
                    ContentValues cv = new ContentValues();
                    cv.put(MediaStore.MediaColumns.DISPLAY_NAME, out.getName());
                    cv.put(MediaStore.MediaColumns.RELATIVE_PATH,
                            Environment.DIRECTORY_DOWNLOADS + "/dsh-export");
                    cv.put(MediaStore.MediaColumns.MIME_TYPE, "application/octet-stream");
                    Uri uri = getContentResolver().insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, cv);
                    if (uri != null) {
                        try (InputStream in = new FileInputStream(src);
                             OutputStream os = getContentResolver().openOutputStream(uri)) {
                            copyStream(in, os);
                        }
                        Toast.makeText(this, getString(R.string.extract_ok, rel),
                                Toast.LENGTH_SHORT).show();
                        return true;
                    }
                } catch (Exception e2) {
                    Toast.makeText(this, getString(R.string.extract_fail, e2.getMessage()),
                            Toast.LENGTH_LONG).show();
                    return false;
                }
            }
            Toast.makeText(this, getString(R.string.extract_fail, e1.getMessage()),
                    Toast.LENGTH_LONG).show();
            return false;
        }
    }

    private void copyStream(InputStream in, OutputStream out) throws Exception {
        try (InputStream i = in; OutputStream o = out) {
            byte[] buf = new byte[64 * 1024];
            int n;
            while ((n = i.read(buf)) > 0) o.write(buf, 0, n);
            o.flush();
        }
    }

    // ---------- 删除 ----------

    /** rel 为相对 filesDir 的路径（文件或目录），删除交给 Rust 递归处理。 */
    private void confirmDelete(final String rel, final String displayName) {
        new AlertDialog.Builder(this)
                .setTitle(R.string.dlg_delete)
                .setMessage(getString(R.string.delete_confirm, displayName))
                .setPositiveButton(R.string.dlg_delete, new DialogInterface.OnClickListener() {
                    @Override public void onClick(DialogInterface dialog, int which) {
                        boolean ok = NativeBridge.dshDeletePath(
                                new File(getFilesDir(), rel).getAbsolutePath()) == 1;
                        Toast.makeText(ArtifactsActivity.this,
                                ok ? R.string.delete_ok : R.string.delete_fail,
                                Toast.LENGTH_SHORT).show();
                        refresh();
                    }
                })
                .setNegativeButton(R.string.dlg_cancel, null)
                .show();
    }

    // ---------- 工具 ----------

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        if (requestCode == REQ_STORAGE) {
            if (grantResults.length > 0 && grantResults[0] == PackageManager.PERMISSION_GRANTED) {
                if (pendingRel != null) {
                    doExtract(new File(getFilesDir(), pendingRel), pendingRel);
                    pendingRel = null;
                }
            } else {
                Toast.makeText(this, R.string.perm_storage, Toast.LENGTH_LONG).show();
            }
        }
    }

    private static String shortId(String name) {
        int dash = name.lastIndexOf('-');
        return name.substring(Math.max(0, dash + 1), Math.min(name.length(), dash + 9));
    }

    private static String fmtSize(long bytes) {
        if (bytes < 1024) return bytes + " B";
        if (bytes < 1024 * 1024) return String.format(Locale.US, "%.1f KB", bytes / 1024.0);
        if (bytes < 1024L * 1024 * 1024) return String.format(Locale.US, "%.1f MB", bytes / 1048576.0);
        return String.format(Locale.US, "%.2f GB", bytes / 1073741824.0);
    }

    private static String fmtDate(long ts) {
        return new SimpleDateFormat("yyyy-MM-dd HH:mm", Locale.getDefault()).format(new Date(ts));
    }

    private int dp(int v) {
        return Math.round(v * getResources().getDisplayMetrics().density);
    }
}
