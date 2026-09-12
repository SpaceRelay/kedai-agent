// 导出文件保存:
//  - Android:写入应用缓存后经系统分享选单导出(SAF/content:// 语义,插件 fs 路径写入不适用);
//  - 桌面 Tauri:系统保存对话框,用户自由选择导出位置;
//  - 浏览器:优先 File System Access API 弹保存对话框,不可用时回退常规下载。

declare global {
  interface Window {
    showSaveFilePicker?: (options?: {
      suggestedName?: string;
      types?: Array<{ description: string; accept: Record<string, string[]> }>;
    }) => Promise<FileSystemFileHandle>;
  }
}

import { isTauri } from '@tauri-apps/api/core';
import { isAndroidTauri } from './platform';

/**
 * 浏览器常规下载(不弹保存对话框,位置由浏览器决定):
 * 临时 a[download] 触发点击;ObjectURL 延时 1s 回收(立即 revoke 在部分浏览器会截断下载)。
 */
export function downloadBlob(fileName: string, blob: Blob): void {
  const url = URL.createObjectURL(blob);
  try {
    const a = document.createElement('a');
    a.href = url;
    a.download = fileName;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
  } finally {
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
}

/**
 * 保存导出文件,返回是否成功保存。
 * - 返回 false = 用户在保存对话框点了「取消」(调用方不应提示成功/失败);
 * - 抛出异常 = 保存失败(调用方提示错误)。
 */
export async function saveExportFile(fileName: string, content: string): Promise<boolean> {
  if (isAndroidTauri) {
    // Android:插件 dialog.save 返回的是 SAF content:// URI,插件 fs 的 writeTextFile
    // 按文件系统路径 + 作用域校验写入,语义不匹配 → 改走「写缓存 + 系统分享 Intent」。
    // 分享选单由用户选择落点(文件管理器/微信/网盘等),是移动端导出的标准做法。
    try {
      const { emit } = await import('@tauri-apps/api/event');
      await emit('kedai://share-file', { name: fileName, content });
      return true;
    } catch (e) {
      console.warn('[kedai] Android 分享导出失败', e);
      throw e;
    }
  }
  if (isTauri()) {
    // 桌面版:系统保存对话框 → 用户选位置 → 写入所选路径
    const { save } = await import('@tauri-apps/plugin-dialog');
    const { writeTextFile } = await import('@tauri-apps/plugin-fs');
    const path = await save({
      defaultPath: fileName,
      filters: [{ name: 'JSON', extensions: ['json'] }],
    });
    if (!path) return false; // 用户取消
    await writeTextFile(path, content);
    return true;
  }
  // 浏览器回退:优先保存对话框选位置,不可用时常规下载(位置由浏览器决定)
  const blob = new Blob([content], { type: 'application/json' });
  if (window.showSaveFilePicker) {
    try {
      const handle = await window.showSaveFilePicker({
        suggestedName: fileName,
        types: [{ description: 'JSON', accept: { 'application/json': ['.json'] } }],
      });
      const writable = await handle.createWritable();
      await writable.write(blob);
      await writable.close();
      return true;
    } catch (e) {
      // 用户取消 → 视为取消(不报错);其余异常回退常规下载
      if ((e as Error).name === 'AbortError') return false;
      console.warn('保存对话框不可用,回退常规下载', e);
    }
  }
  downloadBlob(fileName, blob);
  return true;
}
