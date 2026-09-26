/**
 * PanNote - 笔记模块
 * Markdown CRUD + 格式工具栏 + 字数统计 + 字体调节
 * v3: 自动保存（debounce 2s）+ 未保存保护 + 保存状态指示
 * v2.4: 录音锚点 + 辅屏联动（noteVoice 事件总线）
 */
(function() {
    'use strict';
    
    const { state, api, eventBus, $, escapeHtml, formatDate } = window.bijian;
    
    // ==================== DOM ====================
    const notesSearch = $('sidebarSearch');
    const noteTitle = $('noteTitle');
    const noteTags = $('noteTags');
    const noteEditor = $('noteEditor');
    const notePreview = $('notePreview');
    const previewToggleBtn = $('previewToggleBtn');
    const exportMdBtn = $('exportMdBtn');
    const saveNoteBtn = $('saveNoteBtn');
    const deleteNoteBtn = $('deleteNoteBtn');
    const newNoteBtn = $('newNoteBtn');
    const captureShotBtn = $('captureShotBtn');
    const wordCountEl = $('wordCount');
    const charCountNoteEl = $('charCountNote');
    const lineCountEl = $('lineCount');
    const readTimeEl = $('readTime');
    const fontSizeLabel = $('fontSizeLabel');
    
    // ==================== 状态 ====================
    let currentNoteId = null;
    let allNotes = [];
    let previewMode = false;
    let editorFontSize = 15;
    
    // 自动保存
    let saveTimer = null;
    let isDirty = false;
    let autoSaveIndicator = null;
    
    // ==================== 初始化 ====================
    function init() {
        // 创建保存状态指示器
        autoSaveIndicator = document.getElementById('autoSaveIndicator');
        if (!autoSaveIndicator && saveNoteBtn) {
            autoSaveIndicator = document.createElement('span');
            autoSaveIndicator.id = 'autoSaveIndicator';
            autoSaveIndicator.style.cssText = 'font-size:12px;color:var(--text-muted);margin-left:8px;opacity:0;transition:opacity .3s';
            saveNoteBtn.parentNode.insertBefore(autoSaveIndicator, saveNoteBtn.nextSibling);
        }
        
        bindEvents();
        loadNotes();
        updateWordCount();
        updateFontSizeLabel();
    }
    
    function bindEvents() {
        // 保存
        saveNoteBtn.addEventListener('click', () => saveNote(false));
        
        // 删除
        deleteNoteBtn.addEventListener('click', deleteNote);
        
        // 新建
        newNoteBtn.addEventListener('click', () => createNewNote());
        
        // 预览切换
        previewToggleBtn.addEventListener('click', togglePreview);
        
        // 导出——点击按钮切换下拉，选项分别调用不同格式
        const exportDropdown = $('exportDropdown');
        function closeExportDropdown() {
            if (exportDropdown) exportDropdown.style.display = 'none';
        }
        if (exportMdBtn && exportDropdown) {
            exportMdBtn.addEventListener('click', (e) => {
                e.stopPropagation();
                const isOpen = exportDropdown.style.display !== 'none';
                exportDropdown.style.display = isOpen ? 'none' : 'flex';
            });
            exportDropdown.querySelectorAll('.fmt-more-item[data-export]').forEach(item => {
                item.addEventListener('click', () => {
                    const fmt = item.dataset.export;
                    closeExportDropdown();
                    if (fmt === 'md') exportMarkdown();
                    else if (fmt === 'html') exportAsHtml();
                    else if (fmt === 'word') exportAsWord();
                });
            });
            document.addEventListener('click', (e) => {
                if (exportDropdown.style.display !== 'none' &&
                    !exportDropdown.contains(e.target) && e.target !== exportMdBtn) {
                    closeExportDropdown();
                }
            });
        }
        // 模板下拉
        const templateBtn = $('templateBtn');
        const templateDropdown = $('templateDropdown');
        function closeTemplateDropdown() {
            if (templateDropdown) templateDropdown.style.display = 'none';
        }
        if (templateBtn && templateDropdown) {
            templateBtn.addEventListener('click', (e) => {
                e.stopPropagation();
                const isOpen = templateDropdown.style.display !== 'none';
                templateDropdown.style.display = isOpen ? 'none' : 'flex';
            });
            templateDropdown.querySelectorAll('.fmt-more-item[data-tmpl]').forEach(item => {
                item.addEventListener('click', () => {
                    insertTemplate(item.dataset.tmpl);
                    closeTemplateDropdown();
                });
            });
            document.addEventListener('click', (e) => {
                if (templateDropdown.style.display !== 'none' &&
                    !templateDropdown.contains(e.target) && e.target !== templateBtn) {
                    closeTemplateDropdown();
                }
            });
            document.addEventListener('keydown', (e) => {
                if (e.key === 'Escape') {
                    closeExportDropdown();
                    closeTemplateDropdown();
                }
            });
        }
        
        // 截图插入
        if (captureShotBtn) captureShotBtn.addEventListener('click', captureAndInsert);
        
        // 编辑器实时：字数统计 + 预览 + 自动保存
        noteEditor.addEventListener('input', () => {
            updateWordCount();
            if (previewMode) renderPreview();
            markDirty();
            triggerAutoSave();
        });
        
        // 标题、标签变化触发自动保存
        noteTitle.addEventListener('input', () => { markDirty(); triggerAutoSave(); });
        noteTags.addEventListener('input', () => { markDirty(); triggerAutoSave(); });
        noteTitle.addEventListener('change', () => { markDirty(); triggerAutoSave(); });
        noteTags.addEventListener('change', () => { markDirty(); triggerAutoSave(); });
        
        // 格式工具栏按钮（常驻 + 下拉）
        document.querySelectorAll('.fmt-btn[data-fmt], .fmt-more-item[data-fmt]').forEach(btn => {
            btn.addEventListener('click', () => {
                applyFormat(btn.dataset.fmt);
                markDirty();
                triggerAutoSave();
                // 若来自下拉项，点击后收起
                if (btn.classList.contains('fmt-more-item')) {
                    closeFmtDropdown();
                }
            });
        });

        // 更多格式下拉展开/收起
        const fmtMoreBtn = $('fmtMoreBtn');
        const fmtMoreDropdown = $('fmtMoreDropdown');
        function closeFmtDropdown() {
            if (fmtMoreDropdown) {
                fmtMoreDropdown.style.display = 'none';
            }
        }
        function toggleFmtDropdown(e) {
            e.stopPropagation();
            if (!fmtMoreDropdown) return;
            const isOpen = fmtMoreDropdown.style.display !== 'none';
            fmtMoreDropdown.style.display = isOpen ? 'none' : 'flex';
        }
        if (fmtMoreBtn) {
            fmtMoreBtn.addEventListener('click', toggleFmtDropdown);
        }
        // 点击外部关闭下拉
        document.addEventListener('click', (e) => {
            if (fmtMoreDropdown && fmtMoreDropdown.style.display !== 'none') {
                if (!fmtMoreDropdown.contains(e.target) && e.target !== fmtMoreBtn) {
                    closeFmtDropdown();
                }
            }
        });
        // Esc 收起
        document.addEventListener('keydown', (e) => {
            if (e.key === 'Escape') closeFmtDropdown();
        });

        
        // 字体大小调节
        $('fontBigger').addEventListener('click', () => changeFontSize(1));
        $('fontSmaller').addEventListener('click', () => changeFontSize(-1));
        
        // 快捷键
        noteEditor.addEventListener('keydown', (e) => {
            if ((e.metaKey || e.ctrlKey) && e.key === 'b') {
                e.preventDefault();
                applyFormat('bold');
            }
            if ((e.metaKey || e.ctrlKey) && e.key === 'i') {
                e.preventDefault();
                applyFormat('italic');
            }
        });

        // Tab 缩进
        noteEditor.addEventListener('keydown', (e) => {
            if (e.key === 'Tab') {
                e.preventDefault();
                const start = noteEditor.selectionStart;
                const end = noteEditor.selectionEnd;
                noteEditor.value = noteEditor.value.slice(0, start) + '  ' + noteEditor.value.slice(end);
                noteEditor.selectionStart = noteEditor.selectionEnd = start + 2;
            }
        });
        
        // 事件总线
        eventBus.on('notes:new', () => createNewNote());
        eventBus.on('notes:open', (id) => openNote(id));
        eventBus.on('notes:refresh', loadNotes);
        eventBus.on('notes:save', () => saveNote(false));

        // v2.4: 辅屏插入转写/纪要到光标处
        eventBus.on('noteVoice:insertToNote', insertTextAtCursor);
    }

    // ==================== 自动保存 ====================
    function markDirty() {
        isDirty = true;
    }
    
    function updateSaveIndicator(text) {
        if (autoSaveIndicator) {
            autoSaveIndicator.textContent = text;
            autoSaveIndicator.style.opacity = text ? '1' : '0';
        }
        const statusBarSave = $('statusBarSave');
        if (statusBarSave) statusBarSave.textContent = text || '就绪';
    }
    
    function triggerAutoSave() {
        if (saveTimer) clearTimeout(saveTimer);
        updateSaveIndicator('编辑中…');
        saveTimer = setTimeout(async () => {
            saveTimer = null;
            await saveNote(true); // silent save
        }, 2000);
    }
    
    function cancelPendingSave() {
        if (saveTimer) {
            clearTimeout(saveTimer);
            saveTimer = null;
        }
    }
    
    function clearDirty() {
        isDirty = false;
        updateSaveIndicator('');
    }
    
    // ==================== 未保存保护 ====================
    function checkUnsavedBeforeLeave(action) {
        if (!isDirty) { action(); return; }
        // 有未保存修改，弹三选一确认
        showUnsavedConfirm(action);
    }
    
    function showUnsavedConfirm(afterConfirm) {
        const existing = document.getElementById('unsavedConfirm');
        if (existing) existing.remove();
        const overlay = document.createElement('div');
        overlay.id = 'unsavedConfirm';
        overlay.style.cssText = 'position:fixed;inset:0;background:rgba(0,0,0,.45);z-index:9999;display:flex;align-items:center;justify-content:center;';
        overlay.innerHTML = `
            <div style="background:var(--bg-primary);border-radius:12px;padding:20px 24px;max-width:380px;box-shadow:0 8px 30px rgba(0,0,0,.25);">
                <div style="font-size:14px;color:var(--text-primary);line-height:1.6;margin-bottom:18px;">当前笔记有未保存的修改，是否保存？</div>
                <div style="display:flex;justify-content:flex-end;gap:10px;">
                    <button id="ucCancel" style="padding:6px 16px;border-radius:8px;border:1px solid var(--border);background:transparent;color:var(--text-secondary);cursor:pointer;font-size:13px;">取消</button>
                    <button id="ucDiscard" style="padding:6px 16px;border-radius:8px;border:1px solid var(--border);background:transparent;color:var(--text-muted);cursor:pointer;font-size:13px;">放弃修改</button>
                    <button id="ucSave" style="padding:6px 16px;border-radius:8px;border:none;background:var(--accent);color:var(--text-on-accent);cursor:pointer;font-size:13px;">保存并继续</button>
                </div>
            </div>`;
        document.body.appendChild(overlay);
        overlay.querySelector('#ucCancel').addEventListener('click', () => overlay.remove());
        overlay.querySelector('#ucDiscard').addEventListener('click', () => {
            overlay.remove();
            cancelPendingSave();
            isDirty = false;
            updateSaveIndicator('');
            afterConfirm();
        });
        overlay.querySelector('#ucSave').addEventListener('click', async () => {
            overlay.remove();
            await saveNote(true); // silent save
            afterConfirm();
        });
    }
    
    // ==================== 笔记列表（供侧边栏渲染用） ====================
    async function loadNotes() {
        try {
            const data = await api('notes');
            allNotes = data.notes || [];
            // 通知侧边栏刷新统一历史
            eventBus.emit('history:refresh');
        } catch (e) {
            console.error('[Notes] 加载失败:', e);
        }
    }
    
    function getCurrentNoteData() {
        return allNotes.find(n => n.id === currentNoteId);
    }

    // ==================== 格式化操作 ====================
    function applyFormat(type) {
        const start = noteEditor.selectionStart;
        const end = noteEditor.selectionEnd;
        const text = noteEditor.value;
        const selected = text.slice(start, end);
        let before = '', after = '', placeholder = '';
        
        switch (type) {
            case 'bold':
                before = '**'; after = '**';
                placeholder = '粗体文字';
                break;
            case 'italic':
                before = '*'; after = '*';
                placeholder = '斜体文字';
                break;
            case 'strike':
                before = '~~'; after = '~~';
                placeholder = '删除线';
                break;
            case 'h1':
                before = '\n# '; after = '\n';
                placeholder = '一级标题';
                break;
            case 'h2':
                before = '\n## '; after = '\n';
                placeholder = '二级标题';
                break;
            case 'h3':
                before = '\n### '; after = '\n';
                placeholder = '三级标题';
                break;
            case 'ul':
                before = '\n- '; after = '\n';
                placeholder = '列表项';
                break;
            case 'ol':
                before = '\n1. '; after = '\n';
                placeholder = '列表项';
                break;
            case 'quote':
                before = '\n> '; after = '\n';
                placeholder = '引用内容';
                break;
            case 'code':
                before = '\n```\n'; after = '\n```\n';
                placeholder = '代码';
                break;
            case 'link':
                before = '['; after = '](https://)';
                placeholder = '链接文字';
                break;
            case 'hr':
                before = '\n\n---\n\n'; after = '';
                placeholder = '';
                break;
            case 'table':
                before = '\n| 列1 | 列2 | 列3 |\n|---|---|---|\n| 内容 | 内容 | 内容 |\n'; after = '\n';
                placeholder = '';
                break;
        }
        
        const insertText = selected || placeholder;
        const newText = text.slice(0, start) + before + insertText + after + text.slice(end);
        noteEditor.value = newText;
        
        // 选中插入的文字（或光标放在末尾）
        const newCursorStart = start + before.length;
        const newCursorEnd = newCursorStart + insertText.length;
        noteEditor.focus();
        noteEditor.setSelectionRange(newCursorStart, newCursorEnd);
        
        updateWordCount();
        if (previewMode) renderPreview();
    }
    
    // ==================== 字体大小 ====================
    function changeFontSize(delta) {
        editorFontSize = Math.max(12, Math.min(24, editorFontSize + delta));
        noteEditor.style.fontSize = editorFontSize + 'px';
        notePreview.style.fontSize = editorFontSize + 'px';
        updateFontSizeLabel();
    }
    
    function updateFontSizeLabel() {
        if (fontSizeLabel) fontSizeLabel.textContent = editorFontSize + 'px';
    }
    
    // ==================== 字数统计 ====================
    function updateWordCount() {
        const text = noteEditor.value || '';
        const chars = text.length;
        
        // 字数统计：中文按字算，英文按词算
        const cjkMatch = text.match(/[\u4e00-\u9fff\u3400-\u4dbf]/g);
        const cjkCount = cjkMatch ? cjkMatch.length : 0;
        const enWords = text.replace(/[\u4e00-\u9fff\u3400-\u4dbf]/g, ' ')
            .trim().split(/\s+/).filter(w => w.length > 0).length;
        const totalWords = cjkCount + enWords;
        
        const lines = text ? text.split('\n').length : 1;
        const readMin = Math.max(1, Math.ceil(totalWords / 300));
        
        if (wordCountEl) wordCountEl.textContent = totalWords + ' 字';
        if (charCountNoteEl) charCountNoteEl.textContent = chars + ' 字符';
        if (lineCountEl) lineCountEl.textContent = lines + ' 行';
        if (readTimeEl) readTimeEl.textContent = '~' + readMin + ' 分钟';
    }
    
    // ==================== 编辑器 ====================
    async function openNote(noteId) {
        // 取消待执行的自动保存
        cancelPendingSave();
        
        // 检查当前笔记是否有未保存修改
        if (isDirty && currentNoteId) {
            // 先保存当前笔记，再加载新笔记
            await saveNote(true);
        }
        
        try {
            const data = await api(`notes/${noteId}`);
            const note = data || (typeof data === 'object' ? data : null);
            if (!note) { showToast('笔记不存在'); return; }
            
            currentNoteId = noteId;
            state.currentNote = noteId;
            state.activeTab = 'notes';
            
            noteTitle.value = note.title || '';
            noteTags.value = formatTags(note.tags).join(', ');
            noteEditor.value = note.content || '';
            
            deleteNoteBtn.style.display = 'inline-flex';
            
            if (previewMode) renderPreview();
            updateWordCount();
            clearDirty();

            // v2.4: 通知辅屏加载本笔记的会话列表
            eventBus.emit('noteVoice:noteChanged', noteId);
            
        } catch (e) {
            showToast('加载笔记失败: ' + e.message);
        }
    }
    
    function createNewNote() {
        checkUnsavedBeforeLeave(() => {
            cancelPendingSave();
            currentNoteId = null;
            noteTitle.value = '';
            noteTags.value = '';
            noteEditor.value = '';
            notePreview.innerHTML = '';
            deleteNoteBtn.style.display = 'none';
            updateWordCount();
            clearDirty();

            // 通知侧边栏更新选中状态
            eventBus.emit('history:refresh');

            // v2.4: 通知辅屏清空（无笔记 → 空态）
            eventBus.emit('noteVoice:noteChanged', null);
        });
    }
    
    // ==================== 截图插入 ====================
    async function captureAndInsert() {
        if (!currentNoteId) {
            showToast('请先新建或打开一条笔记再截图');
            return;
        }
        try {
            if (typeof window.__TAURI__ === 'undefined') {
                showToast('截图功能仅在桌面版可用');
                return;
            }
            const result = await window.__TAURI__.core.invoke('capture_screenshot', { interactive: true });
            const imgPath = result.path;
            if (!imgPath) throw new Error('截图失败');
            
            const md = `![截图](${imgPath})\n\n`;
            const pos = noteEditor.selectionStart ?? noteEditor.value.length;
            noteEditor.value = noteEditor.value.slice(0, pos) + md + noteEditor.value.slice(pos);
            noteEditor.focus();
            noteEditor.setSelectionRange(pos + md.length, pos + md.length);
            if (previewMode) renderPreview();
            updateWordCount();
            markDirty();
            triggerAutoSave();
            showToast('截图已插入');
        } catch (e) {
            showToast('截图失败: ' + e.message);
        }
    }
    
    async function saveNote(silent) {
        // 取消待执行的自动保存（手动保存时不需要再触发）
        cancelPendingSave();
        
        const title = noteTitle.value.trim() || '无标题';
        const content = noteEditor.value;
        const tags = noteTags.value.split(',').map(t => t.trim()).filter(t => t).join(', ');
        
        // 空内容不自动保存
        if (silent && !content.trim() && !title.trim()) return;
        
        if (silent) updateSaveIndicator('保存中…');
        
            try {
                if (currentNoteId) {
                    await api(`notes/${currentNoteId}`, { title, content, tags });
                    if (!silent) showToast('笔记已更新');
                } else {
                    const data = await api('notes', { title, content, tags });
                    currentNoteId = data.id;
                    deleteNoteBtn.style.display = 'inline-flex';
                    if (!silent) showToast('笔记已创建');
                    // v2.5: 新建笔记已落库 → 通知辅屏同步 note_id（否则录音仍被拦截）
                    eventBus.emit('noteVoice:noteChanged', currentNoteId);
                }

                // v2.4: 保存后同步标题到挂载会话（静默，失败不阻断）
                if (currentNoteId) {
                    try {
                        await api('meetings/sync_note_title', { note_id: currentNoteId, title });
                    } catch(e) { /* 同步标题失败不阻断保存 */ }
                }

                clearDirty();
            if (silent) updateSaveIndicator('已保存');
            else updateSaveIndicator('已保存');
            setTimeout(() => updateSaveIndicator(''), 2000);
            loadNotes();
        } catch (e) {
            updateSaveIndicator('保存失败');
            showToast('保存失败: ' + e.message + '（请手动重试）');
        }
    }
    
    // v2.5: 录音/上传前确保有当前笔记（新建状态下先静默落库拿 note_id）
    // 供 note-voice.js 调用；返回当前 note_id，失败返回 null
    async function ensureNote() {
        if (currentNoteId) return currentNoteId;
        // 编辑器有内容 → 静默保存（saveNote 创建分支会落库并发事件）
        await saveNote(true);
        if (currentNoteId) return currentNoteId;
        // 空内容空标题时 saveNote 会跳过 → 强制落库一条“无标题”
        try {
            const data = await api('notes', { title: '无标题', content: '', tags: '' });
            currentNoteId = data.id;
            deleteNoteBtn.style.display = 'inline-flex';
            clearDirty();
            eventBus.emit('noteVoice:noteChanged', currentNoteId);
            loadNotes();
            return currentNoteId;
        } catch (e) {
            console.error('[Notes] ensureNote 创建失败:', e);
            return null;
        }
    }

    async function deleteNote() {
        if (!currentNoteId) return;
        if (typeof window.bijian.showCustomConfirm === 'function') {
            window.bijian.showCustomConfirm('确定要删除这条笔记吗？删除后不可恢复。', async () => {
                try {
                    if (typeof window.__TAURI__ !== 'undefined') {
                        await window.__TAURI__.core.invoke('delete_note', { noteId: currentNoteId });
                    } else {
                        await fetch(`/api/notes/${currentNoteId}`, { method: 'DELETE' });
                    }
                    showToast('笔记已删除');
                    createNewNote();
                    loadNotes();
                } catch (e) {
                    showToast('删除失败: ' + e.message);
                }
            });
        } else {
            // 兜底：浏览器环境原生 confirm
            if (!confirm('确定要删除这条笔记吗？')) return;
            try {
                if (typeof window.__TAURI__ !== 'undefined') {
                    await window.__TAURI__.core.invoke('delete_note', { noteId: currentNoteId });
                } else {
                    await fetch(`/api/notes/${currentNoteId}`, { method: 'DELETE' });
                }
                showToast('笔记已删除');
                createNewNote();
                loadNotes();
            } catch (e) {
                showToast('删除失败: ' + e.message);
            }
        }
    }
    
    // ==================== 预览 ====================
    function togglePreview() {
        previewMode = !previewMode;
        if (previewMode) {
            noteEditor.style.display = 'none';
            notePreview.style.display = 'block';
            previewToggleBtn.textContent = '编辑';
            renderPreview();
        } else {
            noteEditor.style.display = 'block';
            notePreview.style.display = 'none';
            previewToggleBtn.textContent = '预览';
        }
    }
    
    function renderPreview() {
        const md = noteEditor.value || '';
        if (typeof marked !== 'undefined' && marked.parse) {
            const unsafe = marked.parse(md, { gfm: true, breaks: true, async: false, headerIds: false, mangle: false, sanitize: false });
            // 使用全局统一 sanitizer（与 chat.js 共享同一套清洗规则）
            const cleaned = window.bijian?.sanitizeHtml ? window.bijian.sanitizeHtml(unsafe) : _localSanitize(unsafe);
            notePreview.innerHTML = cleaned;
            // v2.4: 渲染锚点为可点元素
            renderAnchorsInPreview();
        } else {
            notePreview.innerHTML = escapeHtml(md).replace(/\n/g, '<br>');
        }
    }

    /**
     * v2.4: 预览态把文本里的 ⏱ mm:ss 变成可点击的跳转标记
     * marked 渲染后锚点可能被包在 <p> 里，需扫描 text node 替换
     */
    function renderAnchorsInPreview() {
        if (!notePreview) return;
        const anchorRegex = /⏱\s*(\d{1,2}:\d{2})/g;
        const walker = document.createTreeWalker(notePreview, NodeFilter.SHOW_TEXT, null);
        const nodes = [];
        let node;
        while ((node = walker.nextNode())) {
            if (anchorRegex.test(node.nodeValue)) {
                nodes.push(node);
                anchorRegex.lastIndex = 0; // reset
            }
        }
        nodes.forEach(textNode => {
            const text = textNode.nodeValue;
            const parent = textNode.parentNode;
            anchorRegex.lastIndex = 0;
            let match;
            let lastIdx = 0;
            const frag = document.createDocumentFragment();
            while ((match = anchorRegex.exec(text)) !== null) {
                // 前面的普通文本
                if (match.index > lastIdx) {
                    frag.appendChild(document.createTextNode(text.slice(lastIdx, match.index)));
                }
                // 锚点标记 → 可点 span
                const span = document.createElement('span');
                span.className = 'note-anchor';
                span.textContent = '⏱ ' + match[1];
                span.dataset.anchorTime = match[1];
                span.title = '点击跳转到 ' + match[1];
                span.addEventListener('click', () => {
                    const parts = match[1].split(':').map(Number);
                    const sec = parts.length === 2 ? parts[0] * 60 + parts[1] : 0;
                    eventBus.emit('noteVoice:seek', sec);
                });
                frag.appendChild(span);
                lastIdx = match.index + match[0].length;
            }
            if (lastIdx < text.length) {
                frag.appendChild(document.createTextNode(text.slice(lastIdx)));
            }
            parent.replaceChild(frag, textNode);
        });
    }

    // 本地兜底 sanitizer（当全局不可用时）
    function _localSanitize(unsafeHtml) {
        if (!unsafeHtml) return '';
        const tpl = document.createElement('template'); tpl.innerHTML = unsafeHtml;
        tpl.content.querySelectorAll('script,style,iframe,object,embed,svg,form,input,button,link,meta').forEach(n => n.remove());
        tpl.content.querySelectorAll('*').forEach(el => {
            Array.from(el.attributes || []).forEach(attr => {
                const n = attr.name.toLowerCase();
                const v = (attr.value || '').trim().toLowerCase();
                if (n.startsWith('on')) { el.removeAttribute(attr.name); }
                else if (['href','src','xlink:href','action','formaction','background','poster','data','cite'].includes(n) &&
                         (v.startsWith('javascript:') || v.startsWith('data:') || v.startsWith('vbscript:'))) {
                    el.removeAttribute(attr.name);
                }
                else if (n === 'style' && (v.includes('expression') || v.includes('javascript'))) {
                    el.removeAttribute(attr.name);
                }
            });
        });
        return new XMLSerializer().serializeToString(tpl.content);
    }
    
    // ==================== 模板 ====================
    const TEMPLATES = {
        'meeting-notes': `# 会议纪要\n\n**日期：** ${new Date().toLocaleDateString('zh-CN')}  \n**参会人：**   \n**缺席人：**   \n**记录人：**   \n\n## 会议议题\n1. \n2. \n\n## 讨论内容\n\n| 议题 | 讨论纪要 | 结论 |\n|---|---|---|\n| | | |\n\n## 决议事项\n- \n\n## 行动项\n| 序号 | 事项 | 负责人 | 截止日期 | 状态 |\n|---|---|---|---|---|\n| 1 | | | | 待启动 |\n`,
        'weekly-report': `# 周工作汇报\n\n**汇报人：**   \n**汇报周期：** ${new Date().toLocaleDateString('zh-CN')} —  \n\n## 本周工作完成情况\n| 序号 | 工作内容 | 完成情况 | 备注 |\n|---|---|---|---|\n| 1 | | | |\n\n## 待解决问题\n- \n\n## 下周工作计划\n| 序号 | 工作内容 | 计划完成时间 |\n|---|---|---|\n| 1 | | |\n`,
        'action-items': `# 行动项清单\n\n> 最后更新：${new Date().toLocaleString('zh-CN')}\n\n## 进行中\n- [ ] \n- [ ] \n\n## 已完成\n- [x] \n\n## 待启动\n- [ ] \n`,
        'empty-h1': `# \n\n`
    };

    function insertTemplate(name) {
        const tpl = TEMPLATES[name];
        if (!tpl) return;
        const pos = noteEditor.selectionStart ?? noteEditor.value.length;
        const before = noteEditor.value.slice(0, pos);
        const after = noteEditor.value.slice(pos);
        const insert = (before && !before.endsWith('\n') ? '\n\n' : '') + tpl;
        noteEditor.value = before + insert + after;
        const cursorPos = before.length + insert.length;
        noteEditor.focus();
        noteEditor.setSelectionRange(cursorPos, cursorPos);
        updateWordCount();
        if (previewMode) renderPreview();
        markDirty();
        triggerAutoSave();
        showToast('模板已插入');
    }

    // ==================== 导出 ====================
    function exportMarkdown() {
        const title = noteTitle.value.trim() || '无标题';
        const content = noteEditor.value || '';
        if (!content.trim()) { showToast('笔记内容为空'); return; }
        
        const meta = `---\ntitle: ${title}\ndate: ${new Date().toISOString()}\ntags: ${noteTags.value || ''}\n---\n\n`;
        const fullMd = meta + content;
        
        const blob = new Blob([fullMd], { type: 'text/markdown;charset=utf-8' });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = title.replace(/[\\/:*?"<>|]/g, '_') + '.md';
        a.click();
        URL.revokeObjectURL(url);
        showToast('已导出 ' + a.download);
    }

    function getExportHtml() {
        const title = noteTitle.value.trim() || '无标题';
        const content = noteEditor.value || '';
        const tags = noteTags.value || '';
        const html = (typeof marked !== 'undefined') ? marked.parse(content) : content;
        const css = `body{font-family:-apple-system,'PingFang SC',sans-serif;max-width:780px;margin:40px auto;padding:0 24px;color:#222;line-height:1.8}h1{font-size:26px;border-bottom:2px solid #eee;padding-bottom:8px}h2{font-size:20px;margin-top:28px}h3{font-size:16px}blockquote{border-left:3px solid #ddd;margin:0;padding:4px 16px;color:#666;background:#f9f9f9}table{border-collapse:collapse;width:100%;margin:12px 0}th,td{border:1px solid #ddd;padding:8px 12px;text-align:left}th{background:#f5f5f5}code{background:#f4f4f4;padding:2px 6px;border-radius:3px}code,pre{font-family:'SF Mono',monospace}pre{background:#f6f8fa;padding:16px;border-radius:8px;overflow-x:auto}img{max-width:100%}hr{border:none;border-top:1px solid #eee;margin:24px 0}`;
        return `<!DOCTYPE html>\n<html lang="zh-CN">\n<head>\n<meta charset="UTF-8">\n<meta name="viewport" content="width=device-width,initial-scale=1">\n<title>${escapeHtml(title)}</title>\n<style>${css}</style>\n</head>\n<body>\n<h1>${escapeHtml(title)}</h1>\n${html}\n</body>\n</html>`;
    }

    function exportAsHtml() {
        const title = noteTitle.value.trim() || '无标题';
        if (!noteEditor.value.trim()) { showToast('笔记内容为空'); return; }
        const html = getExportHtml();
        const blob = new Blob([html], { type: 'text/html;charset=utf-8' });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = title.replace(/[\\/:*?"<>|]/g, '_') + '.html';
        a.click();
        URL.revokeObjectURL(url);
        showToast('已导出 ' + a.download);
    }

    function exportAsWord() {
        const title = noteTitle.value.trim() || '无标题';
        if (!noteEditor.value.trim()) { showToast('笔记内容为空'); return; }
        const html = getExportHtml();
        // Word 可以直接打开 HTML，只需 MIME 声明为 msword
        const wordHtml = html.replace('</head>', '<!--[if gte mso 9]><xml><w:WordDocument><w:View>Print</w:View></w:WordDocument></xml><![endif]--></head>');
        const blob = new Blob(['\ufeff', wordHtml], { type: 'application/msword;charset=utf-8' });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = title.replace(/[\\/:*?"<>|]/g, '_') + '.doc';
        a.click();
        URL.revokeObjectURL(url);
        showToast('已导出 ' + a.download);
    }
    
    // ==================== v2.4: 录音锚点 ====================
    /**
     * 在编辑器当前光标行末插入 ⏱ mm:ss 锚点标记
     * 仅录音中调用（note-voice.js 换行时触发）
     */
    function insertAnchor(seconds) {
        const pos = noteEditor.selectionStart;
        const text = noteEditor.value;
        // 找当前行末（下一个换行或文本末尾）
        let lineEnd = text.indexOf('\n', pos);
        if (lineEnd === -1) lineEnd = text.length;
        // 在行末插入锚点（如果行末已有锚点则跳过）
        const lineText = text.substring(text.lastIndexOf('\n', pos - 1) + 1, lineEnd);
        if (lineText.includes('⏱')) return; // 同行已有锚点，不重复
        const anchor = ` ⏱ ${fmtAnchorTime(seconds)}`;
        noteEditor.value = text.slice(0, lineEnd) + anchor + text.slice(lineEnd);
        const newPos = lineEnd + anchor.length;
        noteEditor.focus();
        noteEditor.setSelectionRange(newPos, newPos);
        updateWordCount();
        if (previewMode) renderPreview();
        markDirty();
        triggerAutoSave();
    }

    function fmtAnchorTime(sec) {
        const s = Math.max(0, Math.floor(sec || 0));
        const m = Math.floor(s / 60);
        const r = s % 60;
        return `${m.toString().padStart(2,'0')}:${r.toString().padStart(2,'0')}`;
    }

    /**
     * 在编辑器光标处插入任意文本（辅屏"插入转写/纪要"用）
     */
    function insertTextAtCursor(text) {
        if (!text) return;
        const pos = noteEditor.selectionStart ?? noteEditor.value.length;
        const before = noteEditor.value.slice(0, pos);
        const after = noteEditor.value.slice(pos);
        // 前后补换行
        const prefix = (before && !before.endsWith('\n')) ? '\n\n' : '';
        const suffix = (after && !after.startsWith('\n')) ? '\n' : '';
        noteEditor.value = before + prefix + text + suffix + after;
        const cursorPos = (before + prefix + text + suffix).length;
        noteEditor.focus();
        noteEditor.setSelectionRange(cursorPos, cursorPos);
        updateWordCount();
        if (previewMode) renderPreview();
        markDirty();
        triggerAutoSave();
    }

    // ==================== 工具 ====================
    function formatTags(tags) {
        if (!tags) return [];
        const str = typeof tags === 'string' ? tags : JSON.stringify(tags);
        try {
            const arr = JSON.parse(str);
            if (Array.isArray(arr)) return arr.filter(t => t);
        } catch (e) {}
        return str.split(',').map(t => t.trim()).filter(t => t);
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
    
    // ==================== 事件入口（note-voice.js 调用） ====================
    window.noteNotes = {
        ensureNote,
    };

    // ==================== 启动 ====================
    document.addEventListener('DOMContentLoaded', init);
})();
