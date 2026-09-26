/**
 * PanNote - 统一入口调度器
 * Tab 切换 + 全局事件 + API 封装
 * v2: 侧边栏常驻 + 统一历史列表
 */
(function() {
    'use strict';

    // ==================== API 封装 ====================
    const API_BASE = '/api';
    
    async function api(method, body = null, httpMethod = null) {
        const verb = httpMethod || (body ? 'POST' : 'GET');
        const options = { method: verb, headers: { 'Content-Type': 'application/json' } };
        if (body && verb !== 'GET' && verb !== 'DELETE') options.body = JSON.stringify(body);
        else if (body && (verb === 'POST' || verb === 'PATCH')) options.body = JSON.stringify(body);
        
        const r = await fetch(`${API_BASE}/${method}`, options);
        if (!r.ok) throw new Error(`API ${method} 失败: ${r.status}`);
        return await r.json();
    }

    // ==================== 全局状态 ====================
    const state = {
        activeTab: 'notes',
        activeFilter: 'all', // 'all' | 'chat' | 'voice' | 'notes'
        sessions: [],
        meetings: [],
        notes: [],
        currentSession: null,
        currentNote: null,
        ws: null,
        meetingId: null,
        isRecording: false,
        recordingStartTime: null,
        recordingTimer: null,
        mediaRecorder: null,
        audioChunks: [],
        ollamaOnline: false,
        asrOnline: false,
        backendOnline: false
    };

    // ==================== DOM 引用 ====================
    const $ = id => document.getElementById(id);

    // ==================== 统一历史列表 ====================
    // 三种类型合并展示，每项带类型徽章
    async function refreshHistory() {
        const list = $('historyList');
        list.innerHTML = '<div class="loading">加载中…</div>';
        
        try {
            // 并行加载三种历史
            const [chatData, meetingData, noteData] = await Promise.allSettled([
                api('chat/sessions'),
                api('meetings'),
                api('notes')
            ]);

            const chatItems = chatData.status === 'fulfilled' ? (chatData.value.sessions || []) : [];
            const meetingItems = meetingData.status === 'fulfilled' ? (meetingData.value.meetings || []) : [];
            const noteItems = noteData.status === 'fulfilled' ? (noteData.value.notes || []) : [];

            state.sessions = chatItems;
            state.meetings = meetingItems;
            state.notes = noteItems;

            renderUnifiedHistory();
        } catch (e) {
            list.innerHTML = `<div class="error">加载失败: ${e.message}</div>`;
        }
    }

    function renderUnifiedHistory() {
        const list = $('historyList');
        const searchQuery = $('sidebarSearch') ? $('sidebarSearch').value.trim().toLowerCase() : '';
        const filter = state.activeFilter; // 'all' | 'chat' | 'voice' | 'notes'

        // 构建统一列表
        let items = [];

        // 根据筛选决定加载哪些类型
        const showChat  = filter === 'all' || filter === 'chat';
        const showVoice = filter === 'all' || filter === 'voice';
        const showNotes = filter === 'all' || filter === 'notes';

        if (showChat) {
            state.sessions.forEach(s => {
                items.push({
                    id: s.id,
                    type: 'chat',
                    title: s.title || '对话',
                    meta: formatDate(s.updated_at),
                    sortTime: getTime(s.updated_at),
                    active: s.id === state.currentSession
                });
            });
        }

        if (showVoice) {
            state.meetings.forEach(m => {
                // v2.4: 挂了笔记的录音不在历史列表单独显示（已内嵌于笔记辅屏）
                if (m.note_id) return;
                items.push({
                    id: m.id,
                    type: 'voice',
                    title: m.title || '未命名会议',
                    meta: (m.status === 'completed' ? '✅ ' : '') + formatDate(m.created_at),
                    sortTime: getTime(m.created_at),
                    active: false
                });
            });
        }

        if (showNotes) {
            // v2.4: 计算每条笔记挂载的会话数（用于🎙徽章）
            const meetingCountByNote = {};
            state.meetings.forEach(m => {
                if (m.note_id) {
                    meetingCountByNote[m.note_id] = (meetingCountByNote[m.note_id] || 0) + 1;
                }
            });
            state.notes.forEach(n => {
                const recCount = meetingCountByNote[n.id] || 0;
                items.push({
                    id: n.id,
                    type: 'notes',
                    title: n.title || '无标题',
                    meta: formatDate(n.updated_at) + (recCount > 0 ? ` · 🎙${recCount}` : ''),
                    sortTime: getTime(n.updated_at),
                    active: n.id === state.currentNote,
                    recCount
                });
            });
        }

        // 搜索过滤
        if (searchQuery) {
            items = items.filter(item => 
                item.title.toLowerCase().includes(searchQuery)
            );
        }

        // 按时间倒序
        items.sort((a, b) => b.sortTime - a.sortTime);

        // 当筛选为 all 时，当前 Tab 优先置顶
        if (filter === 'all') {
            items.sort((a, b) => {
                if (a.type === state.activeTab && b.type !== state.activeTab) return -1;
                if (b.type === state.activeTab && a.type !== state.activeTab) return 1;
                return 0;
            });
        }

        if (items.length === 0) {
            list.innerHTML = searchQuery 
                ? '<div class="empty" style="padding:16px;color:var(--text-tertiary);font-size:12px">无匹配结果</div>'
                : '<div class="empty" style="padding:16px;color:var(--text-tertiary);font-size:12px">暂无记录</div>';
            return;
        }

        const typeLabels = { chat: 'AI', voice: '录音', notes: '笔记' };

        list.innerHTML = items.map(item => `
            <div class="history-item ${item.active ? 'active' : ''}" data-id="${item.id}" data-type="${item.type}">
                <span class="history-type-badge ${item.type}">${typeLabels[item.type]}</span>
                <div class="history-item-content">
                    <span class="history-item-title">${escapeHtml(item.title)}</span>
                    <span class="history-item-meta">${item.meta}</span>
                </div>
                <button class="history-item-del" data-del-id="${item.id}" data-del-type="${item.type}" title="删除">✕</button>
            </div>
        `).join('');

        // 绑定点击（打开记录）
        list.querySelectorAll('.history-item').forEach(el => {
            el.addEventListener('click', (e) => {
                // 点删除按钮时不触发打开
                if (e.target.closest('.history-item-del')) return;
                const id = el.dataset.id;
                const type = el.dataset.type;

                // 如果点击的不是当前 Tab，先切换 Tab
                if (type !== state.activeTab) {
                    switchTab(type, true); // true = 不重新加载历史
                }

                // 加载对应记录
                if (type === 'chat') {
                    eventBus.emit('chat:load', id);
                } else if (type === 'voice') {
                    eventBus.emit('voice:load', id);
                } else if (type === 'notes') {
                    eventBus.emit('notes:open', id);
                }
            });
        });

        // 绑定删除按钮（事件冒泡到 history-item 已拦截）
        list.querySelectorAll('.history-item-del').forEach(btn => {
            btn.addEventListener('click', (e) => {
                e.stopPropagation();
                const id = btn.dataset.delId;
                const type = btn.dataset.delType;
                confirmDeleteItem(type, id);
            });
        });
    }

    // ==================== 历史记录删除 ====================
    // Tauri WKWebView 不支持原生 confirm()（静默返回 false），用自定义 DOM 确认弹层

    function showCustomConfirm(msg, onOk) {
        const existing = document.getElementById('customConfirm');
        if (existing) existing.remove();
        const overlay = document.createElement('div');
        overlay.id = 'customConfirm';
        overlay.style.cssText = 'position:fixed;inset:0;background:rgba(0,0,0,.45);z-index:9999;display:flex;align-items:center;justify-content:center;';
        overlay.innerHTML = `
            <div style="background:var(--bg-primary);border-radius:12px;padding:20px 24px;max-width:360px;box-shadow:0 8px 30px rgba(0,0,0,.25);">
                <div style="font-size:14px;color:var(--text-primary);line-height:1.6;margin-bottom:18px;">${msg}</div>
                <div style="display:flex;justify-content:flex-end;gap:10px;">
                    <button id="cfCancel" style="padding:6px 16px;border-radius:8px;border:1px solid var(--border);background:transparent;color:var(--text-secondary);cursor:pointer;font-size:13px;">取消</button>
                    <button id="cfOk" style="padding:6px 16px;border-radius:8px;border:none;background:#e5484d;color:#fff;cursor:pointer;font-size:13px;">删除</button>
                </div>
            </div>`;
        document.body.appendChild(overlay);
        overlay.querySelector('#cfCancel').addEventListener('click', () => overlay.remove());
        overlay.querySelector('#cfOk').addEventListener('click', () => { overlay.remove(); onOk(); });
    }

    async function runDeleteItem(type, id) {
        if (typeof window.__TAURI__ !== 'undefined') {
            const cmd = {
                chat: 'delete_chat_session',
                voice: 'delete_meeting',
                notes: 'delete_note'
            };
            const argKey = type === 'chat' ? 'sessionId' : type === 'voice' ? 'meetingId' : 'noteId';
            await window.__TAURI__.core.invoke(cmd[type], { [argKey]: id });
        } else {
            const path = type === 'chat' ? `chat/sessions/${id}` : type === 'voice' ? `meetings/${id}` : `notes/${id}`;
            await api(path, null, 'DELETE');
        }
    }

    function confirmDeleteItem(type, id) {
        const labels = { chat: '这条对话', voice: '这条会议记录', notes: '这条笔记' };
        const msg = `确定要删除${labels[type]}吗？删除后不可恢复。`;
        showCustomConfirm(msg, async () => {
            try {
                await runDeleteItem(type, id);
                // 轻提示
                const tip = document.createElement('div');
                tip.style.cssText = 'position:fixed;bottom:24px;left:50%;transform:translateX(-50%);background:rgba(0,0,0,.8);color:#fff;padding:8px 16px;border-radius:8px;font-size:13px;z-index:10000;';
                tip.textContent = '已删除';
                document.body.appendChild(tip);
                setTimeout(() => tip.remove(), 1500);
                refreshHistory();
                // 若删除的是当前打开的记录，重置界面
                if (type === 'chat' && state.currentSession === id) { state.currentSession = null; eventBus.emit('chat:new'); }
                if (type === 'voice' && state.currentVoice === id) { state.currentVoice = null; eventBus.emit('voice:new'); }
                if (type === 'notes' && state.currentNote === id) { state.currentNote = null; eventBus.emit('notes:new'); }
            } catch (e) {
                const tip = document.createElement('div');
                tip.style.cssText = 'position:fixed;bottom:24px;left:50%;transform:translateX(-50%);background:#e5484d;color:#fff;padding:8px 16px;border-radius:8px;font-size:13px;z-index:10000;';
                tip.textContent = '删除失败: ' + e.message;
                document.body.appendChild(tip);
                setTimeout(() => tip.remove(), 3000);
            }
        });
    }

    function getTime(ts) {
        if (!ts) return 0;
        if (typeof ts === 'number') return ts;
        const d = new Date(ts);
        return isNaN(d.getTime()) ? 0 : d.getTime() / 1000;
    }

    // ==================== Tab 切换 ====================
    function switchTab(tab, skipHistoryRefresh) {
        state.activeTab = tab;
        
        // 更新按钮状态
        document.querySelectorAll('.tab-btn').forEach(btn => {
            btn.classList.toggle('active', btn.dataset.tab === tab);
        });
        
        // 显示对应内容
        document.querySelectorAll('.tab-content').forEach(tc => {
            tc.classList.toggle('active', tc.id === `tab-${tab}`);
        });
        
        // 更新历史面板标题
        const titles = { chat: 'AI 对话', voice: '录音会议', notes: '笔记' };
        if ($('historyTitle')) $('historyTitle').textContent = titles[tab] || '全部记录';
        
        // 刷新历史列表（除非跳过）
        if (!skipHistoryRefresh) {
            refreshHistory();
        } else {
            renderUnifiedHistory();
        }
    }

    // ==================== 状态栏 ====================
    function setStatus(level, text) {
        $('statusDot').className = 'status-dot ' + level;
        $('statusText').textContent = text;
    }

    // ==================== 服务状态分别展示 ====================
    function updateServiceStatus(ollamaOk, asrOk) {
        state.ollamaOnline = ollamaOk;
        state.asrOnline = asrOk;

        const ollamaEl = $('svcOllama');
        const asrEl = $('svcASR');

        if (ollamaEl) {
            ollamaEl.className = 'svc-badge ' + (ollamaOk ? 'online' : 'offline');
            ollamaEl.textContent = 'Ollama' + (ollamaOk ? '' : ' ✗');
        }
        if (asrEl) {
            asrEl.className = 'svc-badge ' + (asrOk ? 'online' : 'offline');
            asrEl.textContent = 'ASR' + (asrOk ? '' : ' ✗');
        }
    }

    // ==================== 离线 Banner ====================
    function showOfflineBanner(show, msg) {
        let banner = $('offlineBanner');
        if (show) {
            if (!banner) {
                banner = document.createElement('div');
                banner.id = 'offlineBanner';
                banner.className = 'offline-banner';
                document.querySelector('.main').insertAdjacentElement('beforebegin', banner);
            }
            banner.innerHTML = '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><line x1="4.93" y1="4.93" x2="19.07" y2="19.07"/></svg> ' + (msg || '后端服务不可达，请检查 Ollama / ASR 是否已启动');
        } else {
            if (banner) banner.remove();
        }
    }

    // ==================== 健康检查 ====================
    async function checkHealth() {
        try {
            const data = await api('health');
            const services = data.services || {};
            const ollamaOk = services.ollama === 'ok' || services.ollama === true;
            const asrOk = services.asr === 'ok' || services.asr === true || services.whisper === 'ok';
            state.backendOnline = true;
            updateServiceStatus(ollamaOk, asrOk);
            const svcStr = Object.entries(services).map(([k, v]) => `${k}:${v}`).join(' ');
            setStatus('ok', `服务正常 · ${svcStr}`);
            showOfflineBanner(false);
            return true;
        } catch (e) {
            state.backendOnline = false;
            updateServiceStatus(false, false);
            setStatus('error', '后端不可达');
            showOfflineBanner(true);
            return false;
        }
    }

    // ==================== 工具函数 ====================
    function escapeHtml(s) {
        return (s || '').replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
    }

    function formatDate(ts) {
        if (!ts) return '';
        let d;
        if (typeof ts === 'number') {
            d = new Date(ts * 1000);
        } else {
            d = new Date(ts);
        }
        if (isNaN(d.getTime())) return '';
        const now = new Date();
        if (d.toDateString() === now.toDateString()) {
            return d.getHours().toString().padStart(2, '0') + ':' + d.getMinutes().toString().padStart(2, '0');
        }
        return (d.getMonth() + 1) + '/' + d.getDate();
    }

    // ==================== 事件总线 ====================
    const eventBus = {
        listeners: {},
        on(event, fn) {
            if (!this.listeners[event]) this.listeners[event] = [];
            this.listeners[event].push(fn);
        },
        emit(event, data) {
            (this.listeners[event] || []).forEach(fn => fn(data));
        }
    };

    // ==================== 主题切换 ====================
    const themeList = ['purple', 'warm', 'dark'];
    const themeNames = { purple: '浅紫', warm: '暖白', dark: '深色' };
    let themeIndex = Number(localStorage.getItem('pannote-theme') || '0');

    function applyTheme(idx) {
        const theme = themeList[idx];
        if (theme === 'purple') {
            document.body.removeAttribute('data-theme');
        } else {
            document.body.setAttribute('data-theme', theme);
        }
        localStorage.setItem('pannote-theme', String(idx));
        themeIndex = idx;
        const label = $('themeLabel');
        if (label) label.textContent = themeNames[theme];
    }

    // ==================== 初始化 ====================
    function init() {
        // Tab 切换
        document.querySelectorAll('.tab-btn').forEach(btn => {
            btn.addEventListener('click', () => switchTab(btn.dataset.tab));
        });

        // v2.5.1 每日自动备份事件（全局层监听，不限于录音页）
        // 失败强提醒（数据是会议唯一原始记录，9-24 三场丢失的教训）；成功弱提示
        if (typeof window.__TAURI__ !== 'undefined' && window.__TAURI__.event) {
            const globalToast = (msg, long) => {
                const toast = document.createElement('div');
                toast.style.cssText = `
                    position: fixed; bottom: 100px; left: 50%; transform: translateX(-50%);
                    background: rgba(0,0,0,0.8); color: white; padding: 8px 16px;
                    border-radius: 20px; font-size: 14px; z-index: 10000;
                    animation: fadeInOut ${long ? '6s' : '2.5s'} ease;
                `;
                toast.textContent = msg;
                document.body.appendChild(toast);
                setTimeout(() => toast.remove(), long ? 6000 : 2500);
            };
            window.__TAURI__.event.listen('auto_backup_failed', (event) => {
                const d = event.payload || {};
                globalToast(`自动备份失败：${d.reason || '未知原因'}。请检查磁盘空间与备份目录权限`, true);
            });
            window.__TAURI__.event.listen('auto_backup_completed', (event) => {
                const d = event.payload || {};
                const kb = Math.round(((d.audio_meeting_dirs && d.audio_meeting_dirs.bytes) || 0) / 1024);
                globalToast(`今日自动备份完成（db${kb > 0 ? ` + 音频 ${kb}KB` : ''}）`);
            });
        }
        
        // 主题切换
        const themeBtn = $('themeBtn');
        if (themeBtn) {
            themeBtn.addEventListener('click', () => {
                applyTheme((themeIndex + 1) % themeList.length);
            });
        }
        applyTheme(themeIndex); // 恢复上次主题

        // 新建按钮
        $('newItemBtn').addEventListener('click', () => {
            if (state.activeTab === 'chat') eventBus.emit('chat:new');
            else if (state.activeTab === 'voice') eventBus.emit('voice:new');
            else if (state.activeTab === 'notes') eventBus.emit('notes:new');
        });
        
        // 侧边栏搜索
        let searchDebounce = null;
        $('sidebarSearch').addEventListener('input', () => {
            clearTimeout(searchDebounce);
            searchDebounce = setTimeout(() => renderUnifiedHistory(), 200);
        });
        
        // 类型筛选
        document.querySelectorAll('.filter-chip').forEach(chip => {
            chip.addEventListener('click', () => {
                document.querySelectorAll('.filter-chip').forEach(c => c.classList.remove('active'));
                chip.classList.add('active');
                state.activeFilter = chip.dataset.filter;
                renderUnifiedHistory();
            });
        });
        
        // 全局快捷键
        document.addEventListener('keydown', (e) => {
            // Cmd/Ctrl + N: 新建（当前 Tab）
            if ((e.metaKey || e.ctrlKey) && e.key === 'n') {
                e.preventDefault();
                if (state.activeTab === 'chat') eventBus.emit('chat:new');
                else if (state.activeTab === 'voice') eventBus.emit('voice:new');
                else if (state.activeTab === 'notes') eventBus.emit('notes:new');
            }
            // Cmd/Ctrl + S: 保存（笔记模式）
            if ((e.metaKey || e.ctrlKey) && e.key === 's' && state.activeTab === 'notes') {
                e.preventDefault();
                eventBus.emit('notes:save');
            }
            // Cmd/Ctrl + 1/2/3: 切换 Tab
            if ((e.metaKey || e.ctrlKey) && ['1','2','3'].includes(e.key)) {
                e.preventDefault();
                const tabs = ['notes', 'voice', 'chat'];
                switchTab(tabs[parseInt(e.key) - 1]);
            }
        });
        
        // 启动健康检查
        checkHealth();
        setInterval(checkHealth, 30000);
        
        // 监听历史刷新
        eventBus.on('history:refresh', () => refreshHistory());
    }

    // ==================== 启动 ====================
    document.addEventListener('DOMContentLoaded', init);

    // 暴露全局接口
    window.bijian = { state, api, eventBus, switchTab, refreshHistory, setStatus, $, escapeHtml, formatDate, showCustomConfirm, updateServiceStatus, showOfflineBanner };
})();
