/**
 * PanNote - 工作台模块
 * AI 对话 + Function Calling + 进化系统
 */
(function() {
    'use strict';
    
    const { state, api, eventBus, $, escapeHtml } = window.bijian;
    
    // ==================== DOM ====================
    const messagesEl = $('messages');
    const inputEl = $('chatInput');
    const sendBtn = $('chatSendBtn');
    const charCountEl = $('chatCharCount');
    const modelSelect = $('modelSelect');
    const roleSelect = $('roleSelect');
    const chatTitle = $('chatTitle');
    
    // ==================== 状态 ====================
    let isStreaming = false;
    let currentSessionId = null;
    let messages = [];
    let titleGenerated = false;
    let abortController = null; // 用于中止 fetch
    let eventUnlistenFn = null; // 用于取消 Tauri event 监听
    
    // ==================== 初始化 ====================
    function init() {
        bindEvents();
        createSession();
    }
    
    function bindEvents() {
        // 发送 / 停止
        sendBtn.addEventListener('click', () => {
            if (isStreaming) {
                abortStreaming();
            } else {
                send();
            }
        });
        inputEl.addEventListener('keydown', (e) => {
            if (e.key === 'Enter' && !e.shiftKey) {
                e.preventDefault();
                if (!isStreaming) send();
            }
        });
        
        // 输入框自适应
        inputEl.addEventListener('input', () => {
            inputEl.style.height = 'auto';
            inputEl.style.height = Math.min(inputEl.scrollHeight, 200) + 'px';
            charCountEl.textContent = inputEl.value.length;
        });
        
        // 模型切换
        modelSelect.addEventListener('change', () => {
            // 模型切换逻辑
        });
        
        // 角色切换
        roleSelect.addEventListener('change', () => {
            // 角色切换时更新 system prompt
        });
        
        // 事件总线
        eventBus.on('chat:new', createSession);
        eventBus.on('chat:load', loadSession);
    }
    
    // ==================== 会话管理 ====================
    async function createSession() {
        try {
            const data = await api('chat/create', { role: 'chat', title: '新对话' });
            currentSessionId = data.id;
            state.currentSession = data.id;
            messages = [];
            titleGenerated = false;
            messagesEl.innerHTML = '';
            showEmptyState();
            chatTitle.textContent = '新对话';
        } catch (e) {
            console.error('[Chat] 创建会话失败:', e);
        }
    }
    
    async function loadSession(sessionId) {
        try {
            const data = await api(`chat/sessions/${sessionId}`);
            currentSessionId = sessionId;
            state.currentSession = sessionId;
            messages = data.messages || [];
            chatTitle.textContent = data.session?.title || '对话';
            renderMessages();
        } catch (e) {
            console.error('[Chat] 加载会话失败:', e);
        }
    }
    
    // ==================== 渲染 ====================
    function showEmptyState() {
        messagesEl.innerHTML = `
            <div class="empty-state">
                <h1>PanNote Pro</h1>
                <p class="slogan">——为 AI 窒息</p>
            </div>`;
        
        // 绑定示例按钮（已移除示例按钮，保留兼容占位）
        messagesEl.querySelectorAll('.example-btn').forEach(btn => {
            btn.addEventListener('click', () => {
                inputEl.value = btn.dataset.text;
                inputEl.focus();
                inputEl.style.height = 'auto';
                inputEl.style.height = Math.min(inputEl.scrollHeight, 200) + 'px';
                charCountEl.textContent = inputEl.value.length;
            });
        });
    }
    
    function renderMessages() {
        if (messages.length === 0) {
            showEmptyState();
            return;
        }
        messagesEl.innerHTML = '';
        messages.forEach(m => appendMessageDOM(m, false));
        scrollToBottom();
    }
    
    // 共享 marked 渲染选项：彻底禁用原始 HTML → 无需 DOMPurify
    const _BIJIAN_MARKED_OPTS = { gfm: true, breaks: true, async: false, headerIds: false, mangle: false, sanitize: false };
    // 统一 HTML sanitizer：聊天、笔记、会议纪要共用同一套清洗规则
    // 剥离全部危险元素 + 危险属性（on*/href=javascript:/data:/vbscript:/style=expression 等）
    function _bijian_sanitizeHtml(unsafeHtml) {
        if (!unsafeHtml) return '';
        const wrap = document.createElement('template');
        wrap.innerHTML = unsafeHtml;
        const root = wrap.content;
        // 移除危险元素
        root.querySelectorAll('script, style, iframe, object, embed, svg, form, input, button, link, meta').forEach(n => n.remove());
        // 移除危险属性
        root.querySelectorAll('*').forEach(el => {
            Array.from(el.attributes || []).forEach(attr => {
                const n = attr.name.toLowerCase();
                const v = attr.value || '';
                const low = v.trim().toLowerCase();
                if (n.startsWith('on')) { el.removeAttribute(attr.name); return; }
                if (['href','src','xlink:href','action','formaction','background','poster','data','cite'].includes(n)) {
                    if (low.startsWith('javascript:') || low.startsWith('data:') || low.startsWith('vbscript:')) {
                        el.removeAttribute(attr.name);
                    }
                }
                // style 属性中的 expression()/javascript 过滤
                if (n === 'style' && (low.includes('expression') || low.includes('javascript'))) {
                    el.removeAttribute(attr.name);
                }
            });
        });
        return new XMLSerializer().serializeToString(root);
    }

    // 暴露为全局统一 sanitizer
    window.bijian = window.bijian || {};
    window.bijian.sanitizeHtml = _bijian_sanitizeHtml;

    function formatContent(text) {
        if (!text) return '';
        if (typeof marked !== 'undefined' && marked.parse) {
            return _bijian_sanitizeHtml(marked.parse(text, _BIJIAN_MARKED_OPTS));
        }
        return formatStreaming(text);
    }
    
    // 流式渲染：与最终渲染保持一致（用 marked），
    // 但截断不完整的尾部标记，避免闪烁抖动
    function formatStreaming(text) {
        if (!text) return '';
        // 截掉尾部未闭合的标记（```、**、` 等），避免渲染抖动
        let safe = text;
        const backticks = (safe.match(/```/g) || []).length;
        if (backticks % 2 === 1) {
            const idx = safe.lastIndexOf('```');
            safe = safe.slice(0, idx);
        }
        const asterisks = (safe.match(/\*\*/g) || []).length;
        if (asterisks % 2 === 1) {
            const idx = safe.lastIndexOf('**');
            safe = safe.slice(0, idx);
        }
        const singleBacktick = (safe.match(/`/g) || []).length;
        if (singleBacktick % 2 === 1) {
            const idx = safe.lastIndexOf('`');
            safe = safe.slice(0, idx);
        }
        if (typeof marked !== 'undefined' && marked.parse) {
            return _bijian_sanitizeHtml(marked.parse(safe, _BIJIAN_MARKED_OPTS));
        }
        let html = escapeHtml(safe);
        html = html.replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>');
        html = html.replace(/\n/g, '<br>');
        return html;
    }
    
    function appendMessageDOM(msg, streaming = false) {
        const empty = messagesEl.querySelector('.empty-state');
        if (empty) empty.remove();
        
        const wrap = document.createElement('div');
        wrap.className = 'message ' + msg.role;
        
        const bubble = document.createElement('div');
        bubble.className = 'message-bubble' + (streaming ? ' streaming-cursor' : '');
        
        if (msg.role === 'assistant') {
            bubble.innerHTML = formatContent(msg.content);
        } else {
            bubble.textContent = msg.content;
        }
        
        wrap.appendChild(bubble);
        
        // 评分栏 + 操作栏
        if (msg.role === 'assistant' && !streaming && msg.id) {
            const actionBar = createActionBar(msg, bubble);
            wrap.appendChild(actionBar);
            const ratingBar = createRatingBar(msg.id, msg.rating || 0);
            wrap.appendChild(ratingBar);
        }
        
        messagesEl.appendChild(wrap);
        scrollToBottom();
        return { wrap, bubble };
    }
    
    function createActionBar(msg, bubble) {
        const bar = document.createElement('div');
        bar.className = 'action-bar';
        
        // 复制按钮
        const copyBtn = document.createElement('button');
        copyBtn.className = 'action-btn';
        copyBtn.textContent = '复制';
        copyBtn.title = '复制回复内容';
        copyBtn.addEventListener('click', () => {
            navigator.clipboard.writeText(msg.content || '').then(() => {
                copyBtn.textContent = '已复制';
                setTimeout(() => copyBtn.textContent = '复制', 1500);
            }).catch(() => {
                // fallback
                const ta = document.createElement('textarea');
                ta.value = msg.content || '';
                document.body.appendChild(ta);
                ta.select();
                document.execCommand('copy');
                ta.remove();
                copyBtn.textContent = '已复制';
                setTimeout(() => copyBtn.textContent = '复制', 1500);
            });
        });
        
        // 重生成按钮
        const regenBtn = document.createElement('button');
        regenBtn.className = 'action-btn';
        regenBtn.textContent = '重生成';
        regenBtn.title = '重新生成回复';
        regenBtn.addEventListener('click', () => {
            // 找到上一条用户消息
            const msgIndex = messages.findIndex(m => m.id === msg.id);
            let userText = '';
            for (let i = msgIndex - 1; i >= 0; i--) {
                if (messages[i].role === 'user') {
                    userText = messages[i].content;
                    break;
                }
            }
            if (userText) {
                inputEl.value = userText;
                send();
            }
        });
        
        bar.appendChild(copyBtn);
        bar.appendChild(regenBtn);
        return bar;
    }
    
    function createRatingBar(msgId, currentRating) {
        const bar = document.createElement('div');
        bar.className = 'rating-bar';
        
        const thumbsUp = document.createElement('button');
        thumbsUp.className = 'rating-btn thumbs-up' + (currentRating === 1 ? ' active' : '');
        thumbsUp.textContent = '👍';
        thumbsUp.title = '好评';
        thumbsUp.addEventListener('click', () => rateMessage(msgId, currentRating === 1 ? 0 : 1, bar));
        
        const thumbsDown = document.createElement('button');
        thumbsDown.className = 'rating-btn thumbs-down' + (currentRating === -1 ? ' active' : '');
        thumbsDown.textContent = '👎';
        thumbsDown.title = '差评';
        thumbsDown.addEventListener('click', () => rateMessage(msgId, currentRating === -1 ? 0 : -1, bar));
        
        bar.appendChild(thumbsUp);
        bar.appendChild(thumbsDown);
        return bar;
    }
    
    async function rateMessage(msgId, rating, barElement) {
        try {
            await api(`chat/messages/${msgId}/rate`, { rating });
            
            const btns = barElement.querySelectorAll('.rating-btn');
            btns.forEach(b => b.classList.remove('active'));
            
            if (rating === 1) barElement.querySelector('.thumbs-up').classList.add('active');
            else if (rating === -1) barElement.querySelector('.thumbs-down').classList.add('active');
            
            showToast(rating === 1 ? '已标记好评 👍' : rating === -1 ? '已标记差评 👎' : '已取消评分');
        } catch (e) {
            showToast('评分失败');
        }
    }
    
    function scrollToBottom() {
        requestAnimationFrame(() => { messagesEl.scrollTop = messagesEl.scrollHeight; });
    }
    
    function showToast(msg) {
        const toast = document.createElement('div');
        toast.style.cssText = `
            position: fixed; bottom: 100px; left: 50%; transform: translateX(-50%);
            background: rgba(0,0,0,0.8); color: white; padding: 8px 16px;
            border-radius: 20px; font-size: 14px; z-index: 10000;
            animation: fadeInOut 2s ease;
        `;
        toast.textContent = msg;
        document.body.appendChild(toast);
        setTimeout(() => toast.remove(), 2000);
    }
    
    // ==================== 流式中止 ====================
    const SEND_SVG = '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="22" y1="2" x2="11" y2="13"/><polygon points="22 2 15 22 11 13 2 9 22 2"/></svg>';
    const STOP_SVG = '<svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor"><rect x="6" y="6" width="12" height="12" rx="2"/></svg>';
    
    function setSendBtnMode(mode) {
        if (mode === 'stop') {
            sendBtn.innerHTML = STOP_SVG;
            sendBtn.title = '停止生成';
            sendBtn.disabled = false;
            sendBtn.classList.add('stop-mode');
        } else {
            sendBtn.innerHTML = SEND_SVG;
            sendBtn.title = '发送';
            sendBtn.disabled = false;
            sendBtn.classList.remove('stop-mode');
        }
    }
    
    function abortStreaming() {
        if (abortController) {
            try { abortController.abort(); } catch(e) {}
            abortController = null;
        }
        if (eventUnlistenFn) {
            try { eventUnlistenFn(); } catch(e) {}
            eventUnlistenFn = null;
        }
        isStreaming = false;
        setSendBtnMode('send');
        // 移除流式光标
        document.querySelectorAll('.message-bubble.streaming-cursor').forEach(el => {
            el.classList.remove('streaming-cursor');
        });
    }
    
    // ==================== 发送 ====================
    async function send() {
        const text = inputEl.value.trim();
        if (!text || isStreaming) return;
        if (!currentSessionId) await createSession();
        
        // 添加用户消息
        const userMsg = { role: 'user', content: text, ts: Date.now() / 1000 };
        messages.push(userMsg);
        appendMessageDOM({ role: 'user', content: text }, false);
        
        // 自动标题
        if (!titleGenerated) {
            titleGenerated = true;
            generateTitleAsync(text);
        }
        
        isStreaming = true;
        sendBtn.disabled = true;
        inputEl.value = '';
        inputEl.style.height = 'auto';
        charCountEl.textContent = '0';
        
        // 切换为停止按钮
        setSendBtnMode('stop');
        abortController = new AbortController();
        
        const { bubble } = appendMessageDOM({ role: 'assistant', content: '' }, true);
        
        const role = roleSelect.value;
        const enableTools = needsWebSearch(text);
        
        let full = '';
        let fcEvents = [];
        
        const isTauri = typeof window.__TAURI__ !== 'undefined';
        
        try {
            if (isTauri) {
                // ===== Tauri 环境：通过 event 监听流式响应 =====
                const eventName = `chat_stream_${currentSessionId}`;
                let eventUnlisten = null;
                let streamDone = false;
                
                const eventPromise = new Promise((resolve, reject) => {
                    const { listen } = window.__TAURI__.event;
                    listen(eventName, (event) => {
                        const data = event.payload;
                        
                        if (data.error) {
                            bubble.textContent = '出错：' + data.error;
                            bubble.classList.remove('streaming-cursor');
                            streamDone = true;
                            resolve();
                            return;
                        }
                        
                        if (data.type === 'tool_call') {
                            const toolName = data.tool || '';
                            const args = data.arguments || {};
                            const argStr = args.query || JSON.stringify(args).substring(0, 60);
                            fcEvents.push(`⏳ ${toolName}(${argStr})...`);
                            bubble.innerHTML = formatStreaming(full) + fcEvents.map(e => `<div class="fc-status">${e}</div>`).join('');
                            scrollToBottom();
                            return;
                        }
                        
                        if (data.type === 'tool_result') {
                            const toolName = data.tool || '';
                            if (fcEvents.length > 0) {
                                fcEvents[fcEvents.length - 1] = `✅ ${toolName} 完成`;
                            }
                            bubble.innerHTML = formatStreaming(full) + fcEvents.map(e => `<div class="fc-status done">${e}</div>`).join('');
                            scrollToBottom();
                            return;
                        }
                        
                        if (data.delta) {
                            full += data.delta;
                            bubble.innerHTML = formatStreaming(full) + fcEvents.map(e => `<div class="fc-status done">${e}</div>`).join('');
                            scrollToBottom();
                        }
                        
                        if (data.done) {
                            bubble.innerHTML = formatContent(full);
                            bubble.classList.remove('streaming-cursor');
                            streamDone = true;
                            
                            // 添加评分栏 + 操作栏
                            const wrap = bubble.parentElement;
                            if (wrap && data.message_id) {
                                const actionBar = createActionBar({content: full, id: data.message_id}, bubble);
                                wrap.appendChild(actionBar);
                                const ratingBar = createRatingBar(data.message_id, 0);
                                wrap.appendChild(ratingBar);
                            }
                            
                            messages.push({ role: 'assistant', content: full, id: data.message_id, rating: 0 });
                            eventBus.emit('chat:refresh');
                            resolve();
                        }
                    }).then(unlisten => {
                        eventUnlistenFn = unlisten;
                    });
                });
                
                // 发起请求
                const requestBody = {
                    session_id: currentSessionId,
                    message: text,
                    role: role,
                    model: modelSelect.value,
                    stream: true,
                    enable_tools: enableTools
                };
                
                const r = await fetch('/api/chat/sessions/' + currentSessionId + '/messages', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify(requestBody),
                    signal: abortController ? abortController.signal : undefined
                });
                
                if (!r.ok) throw new Error(`HTTP ${r.status}`);
                
                // 等待 event 完成（最多 5 分钟）
                await Promise.race([
                    eventPromise,
                    new Promise((_, reject) => setTimeout(() => reject(new Error('响应超时')), 300000))
                ]);
                
                // 清理 event 监听
                if (eventUnlistenFn) { try { eventUnlistenFn(); } catch(e) {} eventUnlistenFn = null; }
                
            } else {
                // ===== Web 环境：走原始 SSE 流 =====
                const requestBody = {
                    session_id: currentSessionId,
                    message: text,
                    role: role,
                    model: modelSelect.value,
                    stream: true,
                    enable_tools: enableTools
                };
                
                const r = await fetch('/api/chat/sessions/' + currentSessionId + '/messages', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify(requestBody),
                    signal: abortController ? abortController.signal : undefined
                });
                
                if (!r.ok) throw new Error(`HTTP ${r.status}`);
                
                if (r.body && typeof r.body.getReader === 'function') {
                    const reader = r.body.getReader();
                    const decoder = new TextDecoder();
                    let buf = '';
                    
                    while (true) {
                        const { done, value } = await reader.read();
                        if (done) break;
                        buf += decoder.decode(value, { stream: true });
                        const lines = buf.split('\n');
                        buf = lines.pop();
                        
                        for (const line of lines) {
                            if (!line.startsWith('data: ')) continue;
                            const data = line.slice(6).trim();
                            if (data === '[DONE]') break;
                            
                            try {
                                const obj = JSON.parse(data);
                                if (obj.type === 'tool_call') {
                                    const toolName = obj.tool || '';
                                    const args = obj.arguments || {};
                                    const argStr = args.query || JSON.stringify(args).substring(0, 60);
                                    fcEvents.push(`⏳ ${toolName}(${argStr})...`);
                                    bubble.innerHTML = formatStreaming(full) + fcEvents.map(e => `<div class="fc-status">${e}</div>`).join('');
                                    scrollToBottom();
                                    continue;
                                }
                                if (obj.type === 'tool_result') {
                                    const toolName = obj.tool || '';
                                    if (fcEvents.length > 0) {
                                        fcEvents[fcEvents.length - 1] = `✅ ${toolName} 完成`;
                                    }
                                    bubble.innerHTML = formatStreaming(full) + fcEvents.map(e => `<div class="fc-status done">${e}</div>`).join('');
                                    scrollToBottom();
                                    continue;
                                }
                                if (obj.delta) {
                                    full += obj.delta;
                                    bubble.innerHTML = formatStreaming(full) + fcEvents.map(e => `<div class="fc-status done">${e}</div>`).join('');
                                    scrollToBottom();
                                }
                            } catch (e) {}
                        }
                    }
                    try { reader.cancel(); } catch(e) {}
                }
                
                bubble.innerHTML = formatContent(full);
                bubble.classList.remove('streaming-cursor');
                
                // 保存消息
                const saveResult = await api(`chat/sessions/${currentSessionId}/messages`, { message: text });
                const aiMsg = { role: 'assistant', content: full, id: saveResult.message_id, rating: 0 };
                messages.push(aiMsg);
                
                // 添加评分栏 + 操作栏
                const wrap = bubble.parentElement;
                if (wrap) {
                    const actionBar = createActionBar({content: full, id: saveResult.message_id}, bubble);
                    wrap.appendChild(actionBar);
                    const ratingBar = createRatingBar(saveResult.message_id, 0);
                    wrap.appendChild(ratingBar);
                }
                eventBus.emit('chat:refresh');
            }
            
        } catch (e) {
            if (e.name === 'AbortError') {
                // 用户主动中止，不显示错误
                console.log('[Chat] 流式已中止');
            } else {
                console.error('[Chat] 错误:', e);
                bubble.textContent = '出错：' + e.message;
                bubble.classList.remove('streaming-cursor');
            }
        } finally {
            isStreaming = false;
            abortController = null;
            if (eventUnlistenFn) { try { eventUnlistenFn(); } catch(e) {} eventUnlistenFn = null; }
            setSendBtnMode('send');
        }
    }
    
    function needsWebSearch(text) {
        const keywords = ['搜索', '查一下', '查找', '联网', '最新', '现在', 'search', '天气', '股价', '汇率'];
        const lower = text.toLowerCase();
        return keywords.some(kw => lower.includes(kw.toLowerCase()));
    }
    
    async function generateTitleAsync(text) {
        try {
            // 截取前20字符作为标题
            const title = text.length > 20 ? text.substring(0, 20) + '…' : text;
            chatTitle.textContent = title;
            // 持久化到后端
            if (currentSessionId) {
                await api(`chat/sessions/${currentSessionId}`, { title }, 'PATCH');
                // 刷新侧边栏列表
                if (eventBus) {
                    eventBus.emit('history:refresh');
                }
            }
        } catch (e) {}
    }
    
    // ==================== 启动 ====================
    document.addEventListener('DOMContentLoaded', init);
})();
