/**
 * PanNote - 笔记×录音辅屏模块 (note-voice.js)
 * v2.4: 笔记页内录音——主屏笔记 + 辅屏转写/纪要/播放
 *
 * 职责：
 *  - 辅屏显隐（⌘\）与拖拽调比例（30%~80%，比例存 localStorage）
 *  - 会话列表加载与切换（1 笔记 : N 段录音，挂 note_id）
 *  - 录音启停（复用 start_recording/stop_recording，与语音 Tab 互斥）
 *  - 转写实时渲染 + 点击转写句跳音频（点击时间戳 → audio.currentTime）
 *  - 纪要渲染与加载（三档深度，沿用语音 Tab 偏好）
 *  - 插入转写/纪要到笔记光标处
 *  - 录音中隐藏辅屏时显示浮动胶囊
 *  - ⌘R 开停录音（notes.js 委托触发 noteVoice:toggleRecord）
 */
(function() {
    'use strict';

    const { state, api, eventBus, $, escapeHtml } = window.bijian;

    // ==================== DOM ====================
    const notesContainer = $('notesContainer') || document.querySelector('.notes-container');
    const voicePanel = $('noteVoicePanel');
    const splitDragger = $('splitDragger');
    const collapseVoiceBtn = $('collapseVoiceBtn');
    const noteVoiceToggleBtn = $('noteVoiceToggleBtn');
    const noteVoiceToggleLabel = $('noteVoiceToggleLabel');
    const nvHeader = $('nvSessionWrap');
    const nvSessionSelect = $('nvSessionSelect');
    const nvEmptyTitle = $('nvEmptyTitle');
    const nvInsertTranscriptBtn = $('nvInsertTranscriptBtn');
    const nvInsertSummaryBtn = $('nvInsertSummaryBtn');
    const nvHideMeBtn = $('nvHideMeBtn');
    const nvRecordingBar = $('nvRecordingBar');
    const nvRecordingDuration = $('nvRecordingDuration');
    const nvRecStatus = $('nvRecStatus');
    const nvStopBtn = $('nvStopBtn');
    const nvEmpty = $('nvEmpty');
    const nvStartBtn = $('nvStartBtn');
    const nvUploadBtn = $('nvUploadBtn');
    const nvFileInput = $('nvFileInput');
    const nvWorkspace = $('nvWorkspace');
    const nvTabTranscript = $('nvTabTranscript');
    const nvTabSummary = $('nvTabSummary');
    const nvGenSummaryBtn = $('nvGenSummaryBtn');
    const nvTranscriptContent = $('nvTranscriptContent');
    const nvSummaryContent = $('nvSummaryContent');
    const nvPlayBtn = $('nvPlayBtn');
    const nvPlayTime = $('nvPlayTime');
    const nvProgressBarContainer = $('nvProgressBarContainer');
    const nvProgressBarInfo = $('nvProgressBarInfo');
    const nvProgressBarFill = $('nvProgressBarFill');
    const nvProgressQueue = $('nvProgressQueue');
    const recordingPill = $('recordingPill');
    const recordingPillText = $('recordingPillText');
    const noteRecordBtn = $('noteRecordBtn');

    // ==================== 状态 ====================
    let currentNoteId = null;          // 当前笔记 id（由 notes.js 同步）
    let sessions = [];                 // 当前笔记挂载的会话列表
    let currentMeetingId = null;       // 当前辅屏展示的会话 id
    let isRecordingHere = false;       // 录音由本辅屏发起（互斥语音 Tab）
    let recordingStartTime = null;
    let recordingTimer = null;
    let currentView = 'transcript';    // 'transcript' | 'summary'
    let chunkListener = null;
    let micWarningListener = null;
    let nvStatusListener = null;   // v2.5.0 状态机事件（无声录音/部分完成提示）
    let meetingProgressListeners = []; // meeting_progress_{id} 动态监听集合
    let summaryProgressListener = null;
    let audioPlayer = null;
    let audioUrl = null;               // 当前会话音频 asset URL（懒加载）
    let audioLoadPromise = null;

    const SPLIT_KEY = 'pannote-nv-split'; // 辅屏比例存储键

    // ==================== 初始化 ====================
    function init() {
        bindPanelEvents();
        bindHotkeys();
        applySplitFromStorage();
        eventBus.on('noteVoice:noteChanged', onNoteChanged);
        eventBus.on('noteVoice:toggleRecord', toggleRecording);
        eventBus.on('noteVoice:seek', (seconds) => seekAudio(seconds));
    }

    // 笔记切换/新建时刷新辅屏
    async function onNoteChanged(noteId) {
        // 录音中切笔记：先自动停录（会话保留在原笔记下），再切换
        if (isRecordingHere && noteId !== currentNoteId) {
            showToast('已自动停止录音');
            await stopRecording();
        }
        currentNoteId = noteId;
        if (audioPlayer) { try { audioPlayer.pause(); } catch(e) {} }
        audioPlayer = null; audioUrl = null; audioLoadPromise = null;
        unloadListeners();
        currentMeetingId = null;
        sessions = [];
        renderSessionSelect();
        if (noteId) {
            loadSessions(noteId);
        } else {
            showEmpty('录音');
        }
    }

    // ==================== 会话列表 ====================
    async function loadSessions(noteId) {
        try {
            const data = await api(`meetings/by_note?note_id=${encodeURIComponent(noteId)}`);
            sessions = data.meetings || [];
            renderSessionSelect();
            if (sessions.length === 0) {
                showEmpty('录音');
            } else {
                // 默认选中最近一段
                await selectSession(sessions[0].id, { silent: true });
            }
        } catch (e) {
            console.error('[NoteVoice] 加载会话列表失败:', e);
            showEmpty('录音');
        }
    }

    function renderSessionSelect() {
        if (!nvHeader || !nvSessionSelect) return;
        if (sessions.length >= 2) {
            nvHeader.style.display = '';
            nvSessionSelect.innerHTML = sessions.map(s => {
                const label = s.title || ('录音 ' + shortDate(s.created_at));
                return `<option value="${escapeHtml(s.id)}" ${s.id === currentMeetingId ? 'selected' : ''}>${escapeHtml(label)}</option>`;
            }).join('');
        } else {
            nvHeader.style.display = 'none';
        }
        if (nvEmptyTitle) {
            nvEmptyTitle.textContent = sessions.length === 1
                ? (sessions[0].title || '录音')
                : '录音';
        }
        // 标题区域：多会话时切 select 独占，单/零会话时纯文字
        if (nvEmptyTitle) {
            nvEmptyTitle.style.display = sessions.length >= 2 ? 'none' : '';
        }
    }

    async function selectSession(meetingId, opts = {}) {
        const { silent = false, keepView = false } = opts;
        if (currentMeetingId === meetingId && sessions.length && !opts.force) return;
        if (audioPlayer) { try { audioPlayer.pause(); } catch(e) {} }
        audioPlayer = null; audioUrl = null; audioLoadPromise = null;
        unloadListeners();
        currentMeetingId = meetingId;
        nvTranscriptContent.innerHTML = '';
        nvSummaryContent.innerHTML = '';
        renderSessionSelect();
        renderStatusCard(); // v2.5.2 状态卡片（段级统计+引擎+失败重试）

        // 显示工作区
        nvEmpty.style.display = 'none';
        nvWorkspace.style.display = 'flex';
        if (!keepView) { switchView('transcript'); }
        updatePlayTime(0);

        try {
            // 加载转写
            const resp = await api(`meetings/${meetingId}/transcript`);
            const chunks = resp.chunks || [];
            renderTranscriptFull(chunks);
        } catch (e) { /* 无转写 */ }

        // 加载纪要（有则渲染，无则静默）
        loadSummary(false);
    }

    // ==================== 录音启停 ====================
    async function toggleRecording() {
        if (isRecordingHere) {
            await stopRecording();
        } else {
            await startRecording();
        }
    }

    async function startRecording() {
        if (state.isRecording && !isRecordingHere) {
            showToast('语音 Tab 正在录音，请先在那边停止');
            return;
        }
        if (isRecordingHere) return;

        if (typeof window.__TAURI__ === 'undefined') {
            showToast('录音功能仅在桌面版可用');
            return;
        }
        if (!currentNoteId) {
            // v2.5: 新建笔记状态下直接录音 → 先静默落库拿 note_id（不再拦截）
            if (window.noteNotes && typeof window.noteNotes.ensureNote === 'function') {
                try { currentNoteId = await window.noteNotes.ensureNote(); } catch (e) { console.error(e); }
            }
            if (!currentNoteId) {
                showToast('请先新建或打开一条笔记');
                return;
            }
        }

        // 未保存 → 先静默保存拿 note_id（notes.js 的 ensureNote 已保证）
        try {
            // ASR 就绪检查（与语音 Tab 一致）
            showToast('正在检查 ASR 服务...');
            const asrStatus = await api('asr/status');
            const backendsOk = Array.isArray(asrStatus?.backends) && asrStatus.backends.some(b => b.available);
            const runningOk = asrStatus?.running === true;
            if (!backendsOk && !runningOk) {
                showToast('ASR 服务未运行，正在启动...');
                await api('asr/ensure_running', {});
                showToast('ASR 服务已就绪');
            }
        } catch (e) {
            showToast('ASR 状态未知，继续录音');
        }

        try {
            // 创建挂载本笔记的会话，标题与笔记一致
            const noteTitle = getNoteTitle();
            const data = await api('meetings/create', { title: noteTitle, note_id: currentNoteId });
            currentMeetingId = data.id || data.meeting_id;
            sessions.unshift({ id: currentMeetingId, title: noteTitle, created_at: Math.floor(Date.now()/1000) });
            renderSessionSelect();

            // 确保辅屏可见 + 工作区就位
            showPanel(true);
            nvEmpty.style.display = 'none';
            nvWorkspace.style.display = 'flex';
            nvTranscriptContent.innerHTML = '';
            nvSummaryContent.innerHTML = '';
            switchView('transcript');

            // 启动录音（Rust 侧）
            const invoke = window.__TAURI__.core.invoke;
            await invoke('start_recording', { meetingId: currentMeetingId });

            isRecordingHere = true;
            state.isRecording = true;
            state.noteVoiceRecording = true;
            recordingStartTime = Date.now();

            // UI
            nvRecordingBar.style.display = 'flex';
            nvRecStatus.textContent = '监听中…';
            startTimer();
            updateRecordBtnState(true);
            updatePill();

            // 监听实时转写
            const { listen } = window.__TAURI__.event;
            if (chunkListener) { try { chunkListener(); } catch(e) {} }
            chunkListener = await listen('transcript_chunk', (event) => {
                if (!isRecordingHere) return;
                const d = event.payload;
                // 2026-09-11 防幻觉：静音段不入屏，状态栏提示
                if (d.silent) {
                    if (nvRecStatus) nvRecStatus.textContent = `已连续 ${d.silent_streak} 段无声，请检查麦克风权限`;
                    return;
                }
                appendChunk({
                    text: d.text,
                    start_ts: d.start_ts || 0,
                    speaker: d.speaker || 'unknown',
                    chunk_id: d.chunk_id,
                });
                nvRecStatus.textContent = (d.text || '').slice(0, 30);
            });
            // 2026-09-11 防幻觉：连续静音警告（macOS 麦克风权限被拒 → cpal 采集全零）
            if (micWarningListener) { try { micWarningListener(); } catch(e) {} }
            micWarningListener = await listen('mic_permission_warning', (event) => {
                const d = event.payload;
                showToast((d && d.message) || '连续多段无声，请检查麦克风权限');
            });

            // v2.5.0 状态机改真：终态事件提示（无声录音 / 部分完成 / 转写完成）
            if (nvStatusListener) { try { nvStatusListener(); } catch(e) {} }
            nvStatusListener = await listen('meeting_status_changed', (event) => {
                const d = event.payload;
                if (!d) return;
                if (d.status === 'no_voice') {
                    showToast('录音结束：未采集到任何声音（音频全零）。请到 系统设置 → 隐私与安全性 → 麦克风 检查授权后重录');
                } else if (d.status === 'transcription_partial') {
                    showToast(`转写部分完成：成功 ${d.done} 段、失败 ${d.failed} 段（失败段已记录，稍后会自动重试）`);
                } else if (d.status === 'failed') {
                    showToast('转写失败：全部段未成功。请确认 ASR 服务在运行后，重新打开本页自动续转');
                }
                // v2.5.2 fallback 可见：部分片段走了备用引擎（速度回退不再静默）
                if (d.fallback_count > 0) {
                    showToast(`注意：${d.fallback_count} 个片段使用了备用引擎（Paraformer 异常回退 0.6B），速度可能较慢`);
                }
                renderStatusCard();
            });

            // 监听标点恢复（按 chunk_id 定位）
            const progListener = await listen('meeting_progress_' + currentMeetingId, (event) => {
                const d = event.payload;
                if (d.type === 'punctuation_done' && d.chunk_id) {
                    const el = nvTranscriptContent.querySelector(`[data-chunk-id="${d.chunk_id}"] .nv-chunk-text`);
                    if (el && d.punctuated_text) el.textContent = d.punctuated_text;
                }
            });
            meetingProgressListeners.push(progListener);

            showToast('录音开始，边写边录，换行自动埋点');
        } catch (e) {
            console.error('[NoteVoice] 启动录音失败:', e);
            showToast('启动录音失败: ' + (e.message || e));
            resetRecordingUI();
        }
    }

    async function stopRecording() {
        if (!isRecordingHere) return;
        isRecordingHere = false;
        state.isRecording = false;
        delete state.noteVoiceRecording;
        stopTimer();
        nvRecordingBar.style.display = 'none';
        updateRecordBtnState(false);
        updatePill();

        try {
            showToast('录音结束，正在转写最后几段...');
            const invoke = window.__TAURI__.core.invoke;
            const result = await invoke('stop_recording');

            if (chunkListener) { try { chunkListener(); } catch(e) {} chunkListener = null; }

            if (result.segment_count > 0) {
                eventBus.emit('history:refresh');
                if (result.session_dir) {
                    // 纪要流水线照旧（模板/深度沿用语音 Tab 偏好）
                    await generateSummary();
                }
            } else {
                showToast('录音文件为空，请检查麦克风权限');
            }

            // 重新加载会话列表（含新会话）
            await loadSessions(currentNoteId);
            await selectSession(currentMeetingId, { force: true });
        } catch (e) {
            console.error('[NoteVoice] 停止录音失败:', e);
            showToast('停止录音失败: ' + (e.message || e));
        }
    }

    function resetRecordingUI() {
        isRecordingHere = false;
        state.isRecording = false;
        delete state.noteVoiceRecording;
        stopTimer();
        nvRecordingBar.style.display = 'none';
        updateRecordBtnState(false);
        updatePill();
    }

    // 计时
    function startTimer() {
        recordingTimer = setInterval(() => {
            const elapsed = Math.floor((Date.now() - recordingStartTime) / 1000);
            const timeStr = fmtTime(elapsed);
            if (nvRecordingDuration) nvRecordingDuration.textContent = timeStr;
            if (recordingPillText) recordingPillText.textContent = '录音中 ' + timeStr;
        }, 1000);
    }
    function stopTimer() {
        if (recordingTimer) { clearInterval(recordingTimer); recordingTimer = null; }
    }

    // ==================== 锚点支持（notes.js 换行即插，本模块供秒数与跳播） ====================
    function elapsedSeconds() {
        if (!isRecordingHere || !recordingStartTime) return 0;
        return Math.floor((Date.now() - recordingStartTime) / 1000);
    }

    // ==================== 转写渲染（辅屏版：每段独立、带可点时间戳） ====================
    let lastSegIdx = -1;

    function renderTranscriptFull(chunks) {
        nvTranscriptContent.innerHTML = '';
        lastSegIdx = -1;
        if (!chunks || chunks.length === 0) {
            nvTranscriptContent.innerHTML = '<div class="nv-transcript-empty" style="color:var(--text-muted);font-size:12px;padding:8px 0;">暂无转写内容</div>';
            return;
        }
        // 快速段渲染（与语音 Tab 同规则：conf<0.95 为粗转段）
        chunks.filter(c => (c.confidence || 0) < 0.95).forEach(c => {
            const text = c.text || c.transcript || '';
            const speaker = c.speaker || 'unknown';
            const start = c.start_time !== undefined ? c.start_time : (c.start_ts || 0);
            if (!text.trim()) return;
            appendChunk({ text, speaker, start_ts: start, chunk_id: c.id }, { keepScroll: true });
        });
        nvTranscriptContent.scrollTop = 0;
    }

    function appendChunk(data, opts = {}) {
        const text = (data.text || '').trim();
        if (!text) return;
        const start = data.start_ts || 0;
        const chunkId = data.chunk_id || '';

        // 5 分钟分段标记（与语音 Tab 一致）
        const segIdx = Math.floor(start / 300);
        if (segIdx !== lastSegIdx) {
            lastSegIdx = segIdx;
            const marker = document.createElement('div');
            marker.className = 'nv-seg-marker';
            marker.textContent = `${segIdx * 5} 分钟`;
            nvTranscriptContent.appendChild(marker);
        }

        const chunk = document.createElement('div');
        chunk.className = 'nv-chunk';
        if (chunkId) chunk.dataset.chunkId = chunkId;
        chunk.dataset.startTs = String(start);
        chunk.innerHTML = `
            <div class="nv-chunk-meta">
                <span class="nv-chunk-time" title="点击跳转播放">${fmtTime(start)}</span>
                <span class="nv-chunk-speaker">${escapeHtml(speakerDisplay(data.speaker))}</span>
            </div>
            <div class="nv-chunk-text">${escapeHtml(text)}</div>
        `;
        // 点击时间戳跳转播放
        chunk.querySelector('.nv-chunk-time').addEventListener('click', () => seekAudio(start));
        // 点正文也可跳（行业标配）
        chunk.querySelector('.nv-chunk-text').addEventListener('click', () => seekAudio(start));

        nvTranscriptContent.appendChild(chunk);
        if (!opts.keepScroll) {
            nvTranscriptContent.scrollTop = nvTranscriptContent.scrollHeight;
        }
    }

    function speakerDisplay(s) {
        if (!s || s === 'unknown') return '说话人';
        return '说话人 ' + s;
    }

    // ==================== 纪要 ====================
    async function generateSummary(force = false) {
        if (!currentMeetingId) { showToast('无会话，无法生成纪要'); return; }
        // 防重复点击
        if (nvGenSummaryBtn && nvGenSummaryBtn.disabled) return;
        const templateType = window.meetingTemplateSelect?.value || 'meeting';
        const depth = window.summaryDepthSelect?.value || 'standard';
        try {
            // v2.5.2 纪要完整性门槛（前端第一道；服务端 trigger_aggregate 段级门槛兕底）：
            // 0 段拒绝；未完成段拒绝；失败段弹确认流，强制生成时 force=true（服务端注入部分转写标注）
            if (window.__TAURI__ && !force) {
                try {
                    const invoke = window.__TAURI__.core.invoke;
                    const s = await invoke('get_transcription_stats', { meetingId: currentMeetingId });
                    if (s && s.total > 0) {
                        const unfinished = ((s.by_state || {}).pending || 0) + ((s.by_state || {}).processing || 0);
                        if (unfinished > 0) {
                            showToast(`转写尚未完成（${unfinished} 段处理中），请等待完成后再生成纪要`);
                            return;
                        }
                        const failedCnt = (s.by_state || {}).failed || 0;
                        if (failedCnt > 0) {
                            const doneCnt = (s.by_state || {}).done || 0;
                            const ok = window.confirm(`有 ${failedCnt} 个失败段，成功 ${doneCnt} 段。\n\n基于部分转写生成纪要？\n（纪要头部将标注「基于部分转写」；也可先重试失败段）`);
                            if (!ok) return;
                            await doGenerate(templateType, depth, true);
                            return;
                        }
                    }
                } catch (e) { /* 段级查询失败不阻塞：老数据/导入型无段记录，服务端守卫兕底 */ }
            }
            await doGenerate(templateType, depth, false);
        } catch (e) {
            showToast('纪要生成失败：' + (e.message || e));
            restoreGenBtn();
            hideProgress();
        }
    }

    async function doGenerate(templateType, depth, force) {
        // 按钮置为生成中
        if (nvGenSummaryBtn) {
            nvGenSummaryBtn.disabled = true;
            nvGenSummaryBtn.classList.add('generating');
            const label = nvGenSummaryBtn.querySelector('span');
            if (label) label.textContent = '生成中…';
        }
        showProgress('开始生成纪要...', 0);
        try {
        await api(`meetings/${currentMeetingId}/aggregate`, { template_type: templateType, depth: depth, force: force || undefined });

            // 进度监听
            if (window.__TAURI__?.event?.listen) {
                const { listen } = window.__TAURI__.event;
                if (summaryProgressListener) { try { summaryProgressListener(); } catch(e) {} }
                summaryProgressListener = await listen('summary_progress', (event) => {
                    const d = event.payload;
                    if (d.meeting_id !== currentMeetingId) return;
                    if (d.phase === 'start') {
                        showProgress('正在分析转写内容...', 5);
                    } else if (d.phase === 'mapping') {
                        showProgress(`提取要点 ${d.current}/${d.total} 片`, Math.round(10 + (d.current / d.total) * 70));
                        renderProgressQueue(d.current, d.total);
                    } else if (d.phase === 'reducing') {
                        showProgress('汇总生成纪要中...', 85);
                        renderProgressQueue(d.total || 0, d.total || 0, true);
                    } else if (d.phase === 'done') {
                        showProgress('纪要生成完成', 100);
                        setTimeout(() => { hideProgress(); clearProgressQueue(); }, 1200);
                    }
                });
                const mpl = await listen('meeting_progress_' + currentMeetingId, (event) => {
                    const d = event.payload;
                    if (d.type === 'final_summary_ready') {
                        hideProgress();
                        loadSummary(true);
                        restoreGenBtn();
                        if (mpl) { try { mpl(); } catch(e) {} }
                    } else if (d.type === 'final_summary_failed') {
                        hideProgress();
                        clearProgressQueue();
                        showToast('纪要生成失败：' + (d.reason || '无有效转写内容'));
                        restoreGenBtn();
                        if (mpl) { try { mpl(); } catch(e) {} }
                    }
                });
                meetingProgressListeners.push(mpl);
            }

            // 轮询兜底
            let attempts = 0;
            const poll = setInterval(async () => {
                attempts++;
                if (attempts > 90) { clearInterval(poll); hideProgress(); showToast('纪要生成超时'); restoreGenBtn(); return; }
                try {
                    const data = await api(`meetings/${currentMeetingId}/summary`);
                    if (data) {
                        clearInterval(poll);
                        hideProgress();
                        renderSummary(data);
                        restoreGenBtn();
                        if (currentView === 'transcript') switchView('summary');
                    }
                } catch(e) {}
            }, 2000);
        } catch (e) {
            hideProgress();
            showToast('生成失败: ' + e.message);
            restoreGenBtn();
        }
    }

    // ==================== v2.5.2 纪要导出 ====================
    // 路径由系统保存对话框选择，Rust 侧 export_summary 组稿写文件（docx-rs）
    async function exportSummary(format) {
        if (!currentMeetingId) { showToast('无会话，无法导出'); return; }
        try {
            const { save } = window.__TAURI__.dialog;
            const title = (sessions.find(s => s.id === currentMeetingId) || {}).title || '会议纪要';
            const now = new Date();
            const dateStr = `${now.getFullYear()}${String(now.getMonth()+1).padStart(2,'0')}${String(now.getDate()).padStart(2,'0')}`;
            const safeTitle = (title || '纪要').replace(/[\\\\/:*?\"<>|]/g, '_').slice(0, 40);
            const filePath = await save({
                defaultPath: `${safeTitle}_纪要_${dateStr}.${format}`,
                filters: format === 'docx'
                    ? [{ name: 'Word 文档', extensions: ['docx'] }]
                    : [{ name: 'Markdown', extensions: ['md'] }],
            });
            if (!filePath) return; // 用户取消
            const invoke = window.__TAURI__.core.invoke;
            const r = await invoke('export_summary', { meetingId: currentMeetingId, format: format, filePath: filePath });
            showToast('已导出：' + r.path);
            // 打开所在文件夹（shell:allow-open 已授权）
            if (window.__TAURI__?.shell?.open) {
                try {
                    const dir = filePath.substring(0, Math.max(filePath.lastIndexOf('/'), filePath.lastIndexOf('\\')));
                    if (dir) await window.__TAURI__.shell.open(dir);
                } catch(e) {}
            }
        } catch (e) {
            showToast('导出失败：' + (e.message || e));
        }
    }

    function restoreGenBtn() {
        if (!nvGenSummaryBtn) return;
        nvGenSummaryBtn.disabled = false;
        nvGenSummaryBtn.classList.remove('generating');
        const label = nvGenSummaryBtn.querySelector('span');
        if (label) label.textContent = '转纪要';
    }

    // ==================== v2.5.2 转写状态卡片 + 失败段重试 ====================
    // 段级表是唯一事实来源：卡片显示段统计/引擎分布/fallback，failed>0 时出重试按钮
    async function renderStatusCard() {
        const card = $('nvStatusCard');
        if (!card || !currentMeetingId) { if (card) card.style.display = 'none'; return; }
        try {
            const invoke = window.__TAURI__.core.invoke;
            const s = await invoke('get_transcription_stats', { meetingId: currentMeetingId });
            if (!s || !s.total || s.total <= 0) { card.style.display = 'none'; return; } // 导入型/无段记录不显示
            const by = s.by_state || {};
            const done = by.done || 0, silent = by.silent || 0, failed = by.failed || 0;
            const pending = (by.pending || 0) + (by.processing || 0);
            const engines = Object.entries(s.engines || {})
                .map(([e, c]) => e.startsWith('fallback_')
                    ? `${e.replace('fallback_', '备用·')}×${c}`
                    : `${e}×${c}`)
                .join(' · ') || '—';
            const statusMap = {
                'recording': '录音中', 'transcribing': '转写中', 'transcribed': '转写完成',
                'transcription_partial': '部分完成', 'no_voice': '无声录音', 'completed': '纪要完成', 'failed': '转写失败',
            };
            let html = `<div class="nv-sc-row">
                <span class="nv-sc-status">${statusMap[s.status] || s.status || '—'}</span>
                <span class="nv-sc-segs">音频 ${s.total} 段 · 转写 ${done} 成功${silent ? ` · ${silent} 静音` : ''}${failed ? ` · ${failed} 失败` : ''}${pending ? ` · ${pending} 处理中` : ''}</span>
                <span class="nv-sc-engine" title="实际引擎分布">引擎：${escapeHtml(engines)}</span>
            </div>`;
            if (failed > 0) {
                html += `<div class="nv-sc-row nv-sc-actions">
                    <span class="nv-sc-warn">${failed} 个失败段：可重试（音频保留，不丢数据）</span>
                    <button class="nv-retry-btn" id="nvRetryFailedBtn">重试 ${failed} 段</button>
                </div>`;
            }
            if ((s.fallback_count || 0) > 0) {
                html += `<div class="nv-sc-row nv-sc-fallback">注意：${s.fallback_count} 个片段因 Paraformer 异常回退了备用引擎（0.6B），速度较慢但结果已保留</div>`;
            }
            card.innerHTML = html;
            card.style.display = 'block';
            const retryBtn = $('nvRetryFailedBtn');
            if (retryBtn) retryBtn.addEventListener('click', async () => {
                retryBtn.disabled = true; retryBtn.textContent = '重试中…';
                try {
                    const r = await invoke('retry_failed_segments', { meetingId: currentMeetingId });
                    showToast(r.message || '重试已启动');
                    setTimeout(renderStatusCard, 2000);
                } catch (e) {
                    showToast('重试失败：' + e);
                    retryBtn.disabled = false; retryBtn.textContent = `重试 ${failed} 段`;
                }
            });
        } catch (e) {
            card.style.display = 'none'; // 非录音型会议（无段级记录）不显示，不报错
        }
    }

    async function loadSummary(notify) {
        if (!currentMeetingId) return;
        try {
            const data = await api(`meetings/${currentMeetingId}/summary`);
            renderSummary(data);
        } catch (e) {
            if (notify) showToast('暂无纪要');
        }
    }

    function renderSummary(data) {
        if (!nvSummaryContent || !data) return;
        let html = '';
        // v2.5.2 部分转写标注：强制生成的纪要头部警告条（服务端段级门槛 force 放行时注入）
        if (data.partial_note) {
            html += `<div class="nv-summary-partial-warn">${escapeHtml(data.partial_note)}</div>`;
        }
        if (data.tldr) html += `<div class="nv-summary-tldr"><strong>概要：</strong>${escapeHtml(data.tldr)}</div>`;
        if (data.record) {
            html += `<div class="nv-summary-tldr"><strong>${escapeHtml(data.tldr || '记录式纪要')}</strong></div>`;
            const content = data.content || (data.lines || []).join('\n');
            if (content) {
                content.split('\n').forEach(line => {
                    line = line.trim();
                    if (line) html += `<div class="record-line">${escapeHtml(line)}</div>`;
                });
            }
        }
        if (data.key_points && data.key_points.length) {
            html += '<h4>核心结论</h4><ul>';
            data.key_points.forEach(kp => html += `<li>${escapeHtml(kp)}</li>`);
            html += '</ul>';
        }
        if (data.topics && data.topics.length) {
            html += '<h4>议题讨论</h4>';
            data.topics.forEach(t => {
                html += `<div class="nv-summary-topic"><div class="nv-summary-topic-title">${escapeHtml(t.title || '')}</div>`;
                if (t.summary) html += `<div class="nv-summary-topic-text">${escapeHtml(t.summary)}</div>`;
                html += '</div>';
            });
        }
        if (data.decisions && data.decisions.length) {
            html += '<h4>决策</h4><ul>';
            data.decisions.forEach(d => {
                const t = typeof d === 'string' ? d : d.text;
                html += `<li>${escapeHtml(t || '')}</li>`;
            });
            html += '</ul>';
        }
        if (data.actions && data.actions.length) {
            html += '<h4>行动项</h4><ul>';
            data.actions.forEach(a => {
                const t = typeof a === 'string' ? a : a.text;
                const assignee = (a.assignee ? ` — ${escapeHtml(a.assignee)}` : '');
                const deadline = (a.deadline ? ` <span class="nv-action-deadline">截止: ${escapeHtml(a.deadline)}</span>` : '');
                html += `<li>${escapeHtml(t)}${assignee}${deadline}</li>`;
            });
            html += '</ul>';
        }
        if (data.questions && data.questions.length) {
            html += '<h4>待解决问题</h4><ul>';
            data.questions.forEach(q => html += `<li>${escapeHtml(q.text || q)}</li>`);
            html += '</ul>';
        }
        if (data.speaker_notes && data.speaker_notes.length) {
            html += '<h4>各说话人要点</h4>';
            data.speaker_notes.forEach(sn => {
                html += `<div class="nv-speaker-note"><div class="nv-speaker-note-name">${escapeHtml(sn.speaker || '说话人')}</div><ul>`;
                (sn.key_points || []).forEach(kp => html += `<li>${escapeHtml(kp)}</li>`);
                html += '</ul></div>';
            });
        }
        if (data.markdown) html += `<pre class="nv-summary-markdown">${escapeHtml(data.markdown)}</pre>`;
        nvSummaryContent.innerHTML = html || '<div style="color:var(--text-muted);font-size:12px;">暂无纪要</div>';
        // 插入纪要按钮可见性
        if (nvInsertSummaryBtn) nvInsertSummaryBtn.style.display = nvSummaryContent.innerHTML.includes('暂无纪要') ? 'none' : '';
    }

    // ==================== 插入正文 ====================
    function insertTranscriptToNote() {
        if (!currentMeetingId) { showToast('无会话'); return; }
        const lines = [];
        nvTranscriptContent.querySelectorAll('.nv-chunk').forEach(el => {
            const time = el.dataset.startTs || '0';
            const text = el.querySelector('.nv-chunk-text')?.textContent || '';
            const spk = el.querySelector('.nv-chunk-speaker')?.textContent || '';
            lines.push(`[${fmtTime(parseFloat(time))}] ${spk}: ${text}`);
        });
        if (!lines.length) { showToast('无转写内容可插入'); return; }
        eventBus.emit('noteVoice:insertToNote', lines.join('\n'));
        showToast('转写已插入光标处');
    }

    function insertSummaryToNote() {
        if (!currentMeetingId) { showToast('无会话'); return; }
        const text = nvSummaryContent.innerText || '';
        if (!text.trim() || text.trim() === '暂无纪要') { showToast('纪要尚未生成'); return; }
        eventBus.emit('noteVoice:insertToNote', text.trim());
        showToast('纪要已插入光标处');
    }

    // ==================== 播放/跳转 ====================
    async function ensureAudio() {
        if (audioPlayer) return audioPlayer;
        if (!currentMeetingId) return null;
        if (audioLoadPromise) return audioLoadPromise;
        audioLoadPromise = (async () => {
            try {
                const invoke = window.__TAURI__?.core?.invoke;
                if (!invoke) { showToast('播放功能仅在桌面版可用'); return null; }
                const result = await invoke('get_meeting_audio', { meetingId: currentMeetingId });
                if (!result.exists || !result.audio_path) {
                    showToast('未找到录音文件');
                    return null;
                }
                audioUrl = window.__TAURI__?.core?.convertFileSrc
                    ? window.__TAURI__.core.convertFileSrc(result.audio_path)
                    : 'file://' + result.audio_path;
                audioPlayer = new Audio(audioUrl);
                audioPlayer.addEventListener('timeupdate', () => updatePlayTime(audioPlayer.currentTime));
                audioPlayer.addEventListener('ended', () => { updatePlayBtnState(false); updatePlayTime(0); });
                audioPlayer.addEventListener('error', () => { updatePlayBtnState(false); showToast('播放失败：无法加载录音文件'); });
                return audioPlayer;
            } catch (e) {
                console.error('[NoteVoice] 加载音频失败:', e);
                return null;
            } finally {
                audioLoadPromise = null;
            }
        })();
        return audioLoadPromise;
    }

    async function togglePlay() {
        const p = await ensureAudio();
        if (!p) return;
        if (p.paused) { await p.play(); updatePlayBtnState(true); }
        else { p.pause(); updatePlayBtnState(false); }
    }

    // 点击时间戳/正文跳转：先 ensureAudio 再 seek + play
    async function seekAudio(seconds) {
        const p = await ensureAudio();
        if (!p) return;
        if (!isFinite(seconds) || seconds < 0) seconds = 0;
        try { p.currentTime = seconds; } catch(e) {}
        try { await p.play(); updatePlayBtnState(true); } catch(e) {}
    }

    function updatePlayBtnState(playing) {
        if (!nvPlayBtn) return;
        nvPlayBtn.classList.toggle('playing', playing);
        const svg = nvPlayBtn.querySelector('svg');
        if (svg) svg.innerHTML = playing
            ? '<rect x="6" y="4" width="4" height="16"/><rect x="14" y="4" width="4" height="16"/>'
            : '<polygon points="5 3 19 12 5 21 5 3"/>';
    }

    function updatePlayTime(sec) {
        if (!nvPlayTime) return;
        if (!audioPlayer || !audioPlayer.duration || !isFinite(audioPlayer.duration)) {
            nvPlayTime.textContent = sec ? fmtTime(sec) : '';
            return;
        }
        nvPlayTime.textContent = `${fmtTime(sec)} / ${fmtTime(audioPlayer.duration)}`;
    }

    // ==================== 辅屏显隐与拖拽 ====================
    function showPanel(show) {
        if (!notesContainer) return;
        notesContainer.classList.toggle('voice-hidden', !show);
        if (noteVoiceToggleLabel) noteVoiceToggleLabel.textContent = show ? '录音屏' : '录音屏';
        // notes-hidden 时辅助屏不可隐藏（需至少留一屏）
        if (show && notesContainer.classList.contains('notes-hidden')) {
            notesContainer.classList.remove('notes-hidden');
        }
        updatePill();
    }

    function isPanelVisible() {
        return notesContainer && !notesContainer.classList.contains('voice-hidden');
    }

    function togglePanel() {
        showPanel(!isPanelVisible());
    }

    function applySplitFromStorage() {
        try {
            const pct = parseFloat(localStorage.getItem(SPLIT_KEY));
            if (voicePanel && pct >= 30 && pct <= 80) {
                voicePanel.style.width = pct + '%';
            }
        } catch(e) {}
    }

    function bindPanelEvents() {
        if (noteVoiceToggleBtn) noteVoiceToggleBtn.addEventListener('click', togglePanel);
        if (nvHideMeBtn) nvHideMeBtn.addEventListener('click', () => showPanel(false));
        if (collapseVoiceBtn) collapseVoiceBtn.addEventListener('click', (e) => { e.stopPropagation(); showPanel(false); });
        if (recordingPill) recordingPill.addEventListener('click', () => { showPanel(true); });
        if (nvStartBtn) nvStartBtn.addEventListener('click', startRecording);
        if (nvStopBtn) nvStopBtn.addEventListener('click', stopRecording);
        if (nvPlayBtn) nvPlayBtn.addEventListener('click', togglePlay);
        if (nvTabTranscript) nvTabTranscript.addEventListener('click', () => switchView('transcript'));
        if (nvTabSummary) nvTabSummary.addEventListener('click', () => switchView('summary'));
        if (nvGenSummaryBtn) nvGenSummaryBtn.addEventListener('click', () => { switchView('summary'); generateSummary(); });
        // v2.5.2 纪要导出（Word / Markdown）——纪要直接进 OA，不再复制粘贴
        const nvExportDocxBtn = $('nvExportDocxBtn');
        const nvExportMdBtn = $('nvExportMdBtn');
        if (nvExportDocxBtn) nvExportDocxBtn.addEventListener('click', () => exportSummary('docx'));
        if (nvExportMdBtn) nvExportMdBtn.addEventListener('click', () => exportSummary('md'));
        if (nvInsertTranscriptBtn) nvInsertTranscriptBtn.addEventListener('click', insertTranscriptToNote);
        if (nvInsertSummaryBtn) nvInsertSummaryBtn.addEventListener('click', insertSummaryToNote);
        if (nvSessionSelect) nvSessionSelect.addEventListener('change', () => selectSession(nvSessionSelect.value));
        if (nvUploadBtn) nvUploadBtn.addEventListener('click', () => nvFileInput?.click());
        if (nvFileInput) nvFileInput.addEventListener('change', handleUpload);
        if (splitDragger) bindDrag();

        // 工具栏录音按钮（笔记页随手录）
        if (noteRecordBtn) noteRecordBtn.addEventListener('click', toggleRecording);
    }

    // 拖拽调比例（mousedown → mousemove → mouseup）
    function bindDrag() {
        splitDragger.addEventListener('mousedown', (e) => {
            e.preventDefault();
            document.body.classList.add('split-dragging');
            const onMove = (ev) => {
                const rect = notesContainer.getBoundingClientRect();
                // 辅屏在右侧：比例 = (rect.right - ev.clientX) / rect.width * 100
                const pct = (rect.right - ev.clientX) / rect.width * 100;
                const clamped = Math.max(30, Math.min(80, pct));
                if (voicePanel) voicePanel.style.width = clamped + '%';
            };
            const onUp = () => {
                document.body.classList.remove('split-dragging');
                document.removeEventListener('mousemove', onMove);
                document.removeEventListener('mouseup', onUp);
                saveSplit();
            };
            document.addEventListener('mousemove', onMove);
            document.addEventListener('mouseup', onUp);
        });
        // 双击隐藏辅屏
        splitDragger.addEventListener('dblclick', () => showPanel(false));
    }

    function saveSplit() {
        try {
            if (!voicePanel) return;
            const rect = voicePanel.getBoundingClientRect();
            const containerRect = notesContainer.getBoundingClientRect();
            const pct = Math.round((rect.width / containerRect.width) * 100);
            if (pct >= 30 && pct <= 80) localStorage.setItem(SPLIT_KEY, String(pct));
        } catch(e) {}
    }

    // 录音中隐藏辅屏 → 浮动胶囊
    function updatePill() {
        if (!recordingPill) return;
        const show = isRecordingHere && !isPanelVisible();
        recordingPill.style.display = show ? 'flex' : 'none';
    }

    // ==================== 上传音频（挂本笔记） ====================
    async function handleUpload(e) {
        const file = e.target.files[0];
        if (!file) return;
        if (!currentNoteId) {
            // v2.5: 新建笔记状态下上传 → 先静默落库拿 note_id
            if (window.noteNotes && typeof window.noteNotes.ensureNote === 'function') {
                try { currentNoteId = await window.noteNotes.ensureNote(); } catch (e) { console.error(e); }
            }
            if (!currentNoteId) { showToast('请先打开一条笔记'); e.target.value = ''; return; }
        }

        try {
            // 创建挂载会话再上传
            const data = await api('meetings/create', { title: getNoteTitle(), note_id: currentNoteId });
            const mid = data.id || data.meeting_id;
            const invoke = window.__TAURI__?.core?.invoke;
            if (!invoke) { showToast('上传功能仅在桌面版可用'); return; }

            showToast('上传中，转写中...');
            const arrayBuffer = await file.arrayBuffer();
            const uint8 = new Uint8Array(arrayBuffer);
            const home = await window.__TAURI__?.os?.homedir?.() || '';
            const tempDir = home ? `${home}/Library/Application Support/com.bijian.app.pro/tmp_audio` : '/tmp/bijian_audio';
            const tempPath = `${tempDir}/nv_upload_${Date.now()}_${file.name}`;
            const fs = window.__TAURI__?.fs || {};
            try { await fs.mkdir(tempDir, { recursive: true }); } catch(err) {}
            await fs.writeFile(tempPath, Array.from(uint8));
            const result = await invoke('upload_audio', { meetingId: mid, filePath: tempPath });

            if (result.text) {
                showToast('上传成功，转写完成');
            } else {
                showToast('转写结果为空');
            }
            await loadSessions(currentNoteId);
            await selectSession(mid, { force: true });
        } catch (err) {
            showToast('上传失败: ' + (err.message || err));
        }
        e.target.value = '';
    }

    // ==================== 快捷键 ====================
    function bindHotkeys() {
        document.addEventListener('keydown', (e) => {
            // 仅笔记 Tab 生效
            if (state.activeTab !== 'notes') return;
            // ⌘R 开停录音
            if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'r') {
                e.preventDefault();
                toggleRecording();
            }
            // ⌘\ 显隐辅屏
            if ((e.metaKey || e.ctrlKey) && e.key === '\\') {
                e.preventDefault();
                togglePanel();
            }
        });
    }

    // ==================== 视图切换 ====================
    function switchView(view) {
        currentView = view;
        if (view === 'transcript') {
            nvTranscriptContent.style.display = '';
            nvSummaryContent.style.display = 'none';
            nvTabTranscript.classList.add('active');
            nvTabSummary.classList.remove('active');
        } else {
            nvTranscriptContent.style.display = 'none';
            nvSummaryContent.style.display = '';
            nvTabTranscript.classList.remove('active');
            nvTabSummary.classList.add('active');
        }
        // 插入按钮可见性
        if (nvInsertTranscriptBtn) {
            nvInsertTranscriptBtn.style.display = nvTranscriptContent.querySelectorAll('.nv-chunk').length > 0 ? '' : 'none';
        }
    }

    // ==================== 空态 ====================
    function showEmpty(title) {
        nvEmpty.style.display = 'flex';
        nvWorkspace.style.display = 'none';
        nvRecordingBar.style.display = 'none';
        if (title && nvEmptyTitle) nvEmptyTitle.textContent = title;
        if (nvInsertTranscriptBtn) nvInsertTranscriptBtn.style.display = 'none';
        if (nvInsertSummaryBtn) nvInsertSummaryBtn.style.display = 'none';
    }

    // ==================== 监听器清理 ====================
    function unloadListeners() {
        if (chunkListener) { try { chunkListener(); } catch(e) {} chunkListener = null; }
        meetingProgressListeners.forEach(fn => { try { fn(); } catch(e) {} });
        meetingProgressListeners = [];
        if (summaryProgressListener) { try { summaryProgressListener(); } catch(e) {} summaryProgressListener = null; }
    }

    // ==================== 事件入口（notes.js 调用） ====================
    window.noteVoice = {
        elapsedSeconds,
        isRecording: () => isRecordingHere,
        currentMeetingId: () => currentMeetingId,
    };

    // ==================== 工具 ====================
    function getNoteTitle() {
        const el = $('noteTitle');
        return (el && el.value.trim()) || '无标题';
    }

    function fmtTime(seconds) {
        const s = Math.max(0, Math.floor(seconds || 0));
        const m = Math.floor(s / 60);
        const r = s % 60;
        return `${m.toString().padStart(2,'0')}:${r.toString().padStart(2,'0')}`;
    }

    function shortDate(ts) {
        if (!ts) return '';
        const d = new Date(typeof ts === 'number' ? ts * 1000 : ts);
        if (isNaN(d.getTime())) return '';
        return `${(d.getMonth()+1)}/${d.getDate()} ${d.getHours().toString().padStart(2,'0')}:${d.getMinutes().toString().padStart(2,'0')}`;
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

    function showProgress(text, pct) {
        if (!nvProgressBarContainer) return;
        nvProgressBarContainer.style.display = 'block';
        if (text) nvProgressBarInfo.textContent = text;
        if (pct !== undefined && nvProgressBarFill) nvProgressBarFill.style.width = pct + '%';
    }
    function hideProgress() {
        if (!nvProgressBarContainer) return;
        nvProgressBarContainer.style.display = 'none';
        if (nvProgressBarFill) nvProgressBarFill.style.width = '0%';
        if (nvProgressQueue) { nvProgressQueue.innerHTML = ''; nvProgressQueue.style.display = 'none'; }
    }
    function renderProgressQueue(current, total, allDone) {
        if (!nvProgressQueue) return;
        if (!total || total <= 0) { nvProgressQueue.style.display = 'none'; return; }
        nvProgressQueue.style.display = 'flex';
        let html = '<span class="progress-queue-label">分片</span>';
        for (let i = 1; i <= total; i++) {
            let cls = 'pending';
            if (allDone || i <= current) cls = 'done';
            else if (i === current + 1) cls = 'active';
            html += `<span class="progress-queue-item ${cls}"></span>`;
        }
        nvProgressQueue.innerHTML = html;
    }
    function clearProgressQueue() {
        if (!nvProgressQueue) return;
        nvProgressQueue.innerHTML = '';
        nvProgressQueue.style.display = 'none';
    }

    function updateRecordBtnState(recording) {
        if (!noteRecordBtn) return;
        noteRecordBtn.classList.toggle('recording', recording);
        noteRecordBtn.title = recording ? '停止录音 (⌘R)' : '开始录音 (⌘R)';
        // 换图标：麦克风 → 红点方块
        const svg = noteRecordBtn.querySelector('svg');
        if (svg) {
            svg.innerHTML = recording
                ? '<rect x="6" y="6" width="12" height="12" rx="2" fill="currentColor" stroke="none"/>'
                : MIC_SVG_PATH;
        }
    }

    const MIC_SVG_PATH = `
        <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" fill="none"></path>
        <path d="M19 10v2a7 7 0 0 1-14 0v-2" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" fill="none"></path>
        <line x1="12" y1="19" x2="12" y2="23" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"></line>
        <line x1="8" y1="23" x2="16" y2="23" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"></line>
    `;

    // ==================== 启动 ====================
    document.addEventListener('DOMContentLoaded', init);
})();
