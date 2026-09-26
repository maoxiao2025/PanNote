/**
 * PanNote 版本管理脚本
 * 
 * 用法：
 *   node scripts/bump.mjs              # patch +1 (2.3.3 → 2.3.4)
 *   node scripts/bump.mjs minor        # minor +1 (2.3.3 → 2.4.0)
 *   node scripts/bump.mjs major        # major +1 (2.3.3 → 3.0.0)
 *   node scripts/bump.mjs --message "修复录音拦截"  # 自定义提交信息
 *
 * 做了什么：
 *   1. 读取 tauri.conf.json 当前版本号
 *   2. 按规则递增（默认 patch）
 *   3. 同步写入 tauri.conf.json + Cargo.toml + package.json
 *   4. git add 三个文件 + 已有改动 + 提交（commit message 带新版本号）
 *   5. 输出 diff 摘要
 *
 * 设计原则：
 *   - 版本号单一来源 = tauri.conf.json（build.rs 已同步到 Info.plist）
 *   - 改完代码跑一次 bump，版本号 + git 提交一步到位
 *   - 不依赖额外工具，纯 Node.js 标准库
 */
import { readFileSync, writeFileSync } from 'fs';
import { execSync } from 'child_process';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const root = join(__dirname, '..');

// ===== 读取参数 =====
const args = process.argv.slice(2);
const bumpType = args.find(a => ['major', 'minor', 'patch'].includes(a)) || 'patch';
const msgIdx = args.indexOf('--message');
const customMsg = msgIdx >= 0 ? args[msgIdx + 1] : null;

// ===== 工具函数 =====
function readJSON(path) {
  return JSON.parse(readFileSync(path, 'utf-8'));
}

function writeJSON(path, obj) {
  // 保留原有缩进格式（2空格）
  writeFileSync(path, JSON.stringify(obj, null, 2) + '\n');
}

function readTomlVersion(path) {
  const text = readFileSync(path, 'utf-8');
  const m = text.match(/^version\s*=\s*"([^"]+)"/m);
  return m ? m[1] : null;
}

function writeTomlVersion(path, newVersion) {
  let text = readFileSync(path, 'utf-8');
  text = text.replace(/^version\s*=\s*"[^"]+"/m, `version = "${newVersion}"`);
  writeFileSync(path, text);
}

function bumpVersion(v, type) {
  const [major, minor, patch] = v.split('.').map(Number);
  if (type === 'major') return `${major + 1}.0.0`;
  if (type === 'minor') return `${major}.${minor + 1}.0`;
  return `${major}.${minor}.${patch + 1}`;
}

// ===== 主流程 =====
const tauriConfPath = join(root, 'tauri.conf.json');
const cargoPath = join(root, 'Cargo.toml');
const pkgPath = join(root, 'package.json');

// 1. 读当前版本
const tauriConf = readJSON(tauriConfPath);
const currentVersion = tauriConf.version;
const newVersion = bumpVersion(currentVersion, bumpType);

console.log(`版本号: ${currentVersion} → ${newVersion} (${bumpType})`);

// 2. 同步写入三个文件
tauriConf.version = newVersion;
writeJSON(tauriConfPath, tauriConf);

const cargoText = readFileSync(cargoPath, 'utf-8');
const updatedCargo = cargoText.replace(/^version\s*=\s*"[^"]+"/m, `version = "${newVersion}"`);
writeFileSync(cargoPath, updatedCargo);

const pkg = readJSON(pkgPath);
pkg.version = newVersion;
writeJSON(pkgPath, pkg);

// 3. git 提交
const gitDir = root;
const commitMsg = customMsg
  ? `v${newVersion} ${customMsg}`
  : `v${newVersion} 自动版本提交`;

// 先看看有哪些改动（不含 dist/.temp）
const statusOutput = execSync('git status --short', { cwd: gitDir, encoding: 'utf-8' });
const changedFiles = statusOutput
  .split('\n')
  .filter(l => l.trim() && !l.includes('.temp/') && !l.includes('dist/'))
  .map(l => l.trim());

if (changedFiles.length === 0) {
  console.log('无代码改动，仅提交版本号变更');
}

// git add 版本文件 + 所有非 dist/temp 改动
execSync('git add tauri.conf.json Cargo.toml package.json', { cwd: gitDir });
// 也 add 其他已修改的源码文件
const otherFiles = changedFiles
  .map(l => l.replace(/^[MARD?]+\s+/, ''))
  .filter(f => !['tauri.conf.json', 'Cargo.toml', 'package.json'].includes(f));
if (otherFiles.length > 0) {
  execSync(`git add ${otherFiles.map(f => `"${f}"`).join(' ')}`, { cwd: gitDir });
}

// 4. 提交
try {
  execSync(`git commit -m "${commitMsg}"`, { cwd: gitDir, encoding: 'utf-8' });
  console.log(`✓ 提交: ${commitMsg}`);
} catch (e) {
  console.log('提交失败或无变更:', e.message);
}

// 5. 输出摘要
const logOutput = execSync('git log --oneline -3', { cwd: gitDir, encoding: 'utf-8' });
console.log('\n最近提交:');
console.log(logOutput);

const diffStat = execSync('git show --stat HEAD', { cwd: gitDir, encoding: 'utf-8' });
console.log('本次改动:');
console.log(diffStat.split('\n').slice(0, 15).join('\n'));
