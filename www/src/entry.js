// PanNote 前端入口 - 按依赖顺序导入所有模块
// tauri-bridge 必须最先（fetch 拦截），app 其次（全局状态/事件总线），然后是各功能模块
import './tauri-bridge.js';
import './app.js';
import './chat.js';
import './voice.js';
import './note-voice.js';
import './notes.js';
import './hotwords.js';
import './license.js';
