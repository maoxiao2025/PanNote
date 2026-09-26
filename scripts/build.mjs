/**
 * PanNote 前端打包脚本
 * 将 src/ 下的模块按依赖顺序合并打包为 dist/bundle.min.js
 * 
 * 用法:
 *   node scripts/build.mjs          # 单次打包（release）
 *   node scripts/build.mjs --watch  # 监听模式（dev）
 */
import * as esbuild from 'esbuild';
import { existsSync, mkdirSync, copyFileSync, rmSync, readFileSync } from 'fs';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const root = join(__dirname, '..');
const srcDir = join(root, 'www', 'src');
const distDir = join(root, 'www', 'dist');
const isWatch = process.argv.includes('--watch');

// 确保输出目录存在
if (!existsSync(distDir)) mkdirSync(distDir, { recursive: true });

// 单入口（entry.js 按依赖顺序导入所有模块）
const entryPoints = [join(srcDir, 'entry.js')];

// 复制 marked.min.js 到 dist（第三方库不打包，单独引用）
const markedSrc = join(root, 'www', 'marked.min.js');
const markedDest = join(distDir, 'marked.min.js');
if (existsSync(markedSrc)) copyFileSync(markedSrc, markedDest);

// v2.3.2: banner 版本从 package.json 读取，避免每次发版忘记同步
const pkg = JSON.parse(readFileSync(join(root, 'package.json'), 'utf-8'));

/** 构建配置 */
const buildOptions = {
  entryPoints,
  bundle: true,
  minify: !isWatch,           // release 打包压缩，dev 不压缩方便调试
  target: ['safari16'],
  format: 'iife',
  outfile: join(distDir, 'bundle.min.js'),
  sourcemap: isWatch ? 'inline' : false,  // dev 内联 sourcemap，release 不生成
  metafile: true,             // 启用 metafile 以获取准确输出大小
  legalComments: 'none',     // 不保留版权注释
  drop: isWatch ? [] : ['console'],  // release 移除 console
  loader: { '.js': 'js' },
  banner: {
    js: `/* PanNote v${pkg.version} - MIT License */`,
  },
};

async function main() {
  if (isWatch) {
    const ctx = await esbuild.context(buildOptions);
    await ctx.watch();
    console.log('[PanNote] dev 模式已启动，监听文件变化...');
  } else {
    const result = await esbuild.build(buildOptions);
    // 从 metafile 获取准确大小，回退到 stat 文件大小
    let sizeKb = 0;
    const outfile = buildOptions.outfile;
    if (result.metafile?.outputs?.[outfile]?.bytes) {
      sizeKb = result.metafile.outputs[outfile].bytes / 1024;
    } else {
      try {
        const stats = await import('fs').then(fs => fs.statSync(outfile));
        sizeKb = stats.size / 1024;
      } catch (_) { /* ignore */ }
    }
    console.log('[PanNote] 打包完成:');
    console.log(`  输出: ${outfile}`);
    console.log(`  大小: ${sizeKb.toFixed(1)} KB`);
    console.log(`  模式: ${isWatch ? 'dev' : 'release (minified)'}`);
  }
}

main().catch(err => {
  console.error('[PanNote] 打包失败:', err);
  process.exit(1);
});
