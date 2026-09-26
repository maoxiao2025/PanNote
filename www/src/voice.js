/**
 * PanNote - 语音模块（工作台布局版）
 * 三引擎小框 + 主框（转写/纪要切换）
 * v3: 单引擎 Qwen3-ASR 实时转写 + 精转，无交叉校验，说话人分离 + 会议纪要
 */
(function() {
    'use strict';
    
    const { state, api, eventBus, $, escapeHtml } = window.bijian;
    
    // ==================== DOM ====================
    const voiceEmpty = $('voiceEmpty');
    const voiceWorkspace = $('voiceWorkspace');
    const voiceRecordingBar = $('voiceRecordingBar');
    const startRecordBtn = $('startRecordBtn');
    const stopRecordBtn = $('stopRecordBtn');
    const uploadAudioBtn = $('uploadAudioBtn');
    const audioFileInput = $('audioFileInput');
    const recordingDuration = $('recordingDuration');
    const transcriptContent = $('transcriptContent');
    const summaryContent = $('summaryContent');
    const generateSummaryBtn = $('generateSummaryBtn');
    const saveAsNoteBtn = $('saveAsNoteBtn');
    const voiceTitle = $('voiceTitle');
    const meetingTemplateSelect = $('meetingTemplateSelect');
    const summaryDepthSelect = $('summaryDepthSelect');
    const progressBarContainer = $('progressBarContainer');
    const progressBarInfo = $('progressBarInfo');
    const progressBarFill = $('progressBarFill');
    const progressQueue = $('progressQueue');
    const playAudioBtn = $('playAudioBtn');
    const aiCorrectBtn = $('aiCorrectBtn');
    
    // 主框 tab
    const tabTranscript = $('tabTranscript');
    const tabSummary = $('tabSummary');
    
    // ==================== 状态 ====================
    let recordingStartTime = null;
    let recordingTimer = null;
    let meetingId = null;
    let transcriptData = [];
    let chunkListener = null;
    let micWarningListener = null;
    let speakersListener = null;
    let punctuationListener = null;
    let cvDoneListener = null;
    let currentSessionDir = null;
    let currentView = 'transcript'; // 'transcript' | 'summary'
    
    // ==================== 初始化 ====================
    function init() {
        bindEvents();
        bindRefineProgressListener();
    }
    
    function bindEvents() {
        startRecordBtn.addEventListener('click', startRecording);
        stopRecordBtn.addEventListener('click', stopRecording);
        
        uploadAudioBtn.addEventListener('click', () => audioFileInput.click());
        audioFileInput.addEventListener('change', handleFileUpload);
        
        generateSummaryBtn.addEventListener('click', generateSummary);
        saveAsNoteBtn.addEventListener('click', saveAsNote);

        // AI 纠错
        if (aiCorrectBtn) {
            aiCorrectBtn.addEventListener('click', startCorrection);
        }
        
        // 播放录音
        if (playAudioBtn) {
            playAudioBtn.addEventListener('click', togglePlayback);
        }
        
        // 主框 tab 切换
        tabTranscript.addEventListener('click', () => switchView('transcript'));
        tabSummary.addEventListener('click', () => switchView('summary'));
        
        // 语音工作台新建按钮（+ 号，停止后可新增会话重新录音）
        const voiceNewBtn = $('voiceNewBtn');
        if (voiceNewBtn) {
            voiceNewBtn.addEventListener('click', () => newMeeting());
        }
        
        eventBus.on('voice:new', newMeeting);
        eventBus.on('voice:load', loadMeeting);
    }
    
    // ==================== 视图切换 ====================
    function switchView(view) {
        currentView = view;
        if (view === 'transcript') {
            transcriptContent.style.display = '';
            summaryContent.style.display = 'none';
            tabTranscript.classList.add('active');
            tabSummary.classList.remove('active');
        } else {
            transcriptContent.style.display = 'none';
            summaryContent.style.display = '';
            tabTranscript.classList.remove('active');
            tabSummary.classList.add('active');
        }
    }
    
    // ==================== 会议管理 ====================
    let meetingCreated = false;
    
    async function newMeeting() {
        // 如果正在录音，先停止（保存当前会话，再开新会话）
        if (state.isRecording) {
            showToast('正在停止当前录音...');
            try {
                await stopRecording();
            } catch (e) {
                console.error('[Voice] 新建前停止录音失败:', e);
            }
        }
        // 清理旧会话的监听器
        if (chunkListener) { try { chunkListener(); } catch(e) {} chunkListener = null; }
        if (punctuationListener) { try { punctuationListener(); } catch(e) {} punctuationListener = null; }
        if (speakersListener) { try { speakersListener(); } catch(e) {} speakersListener = null; }
        if (cvDoneListener) { try { cvDoneListener(); } catch(e) {} cvDoneListener = null; }
        
        try {
            const data = await api('meetings/create', { title: '新会议 ' + new Date().toLocaleString('zh-CN') });
            meetingId = data.id || data.meeting_id;
            meetingCreated = true;
            voiceTitle.textContent = data.title || '新会议';
            transcriptData = [];
            transcriptContent.innerHTML = '';
            summaryContent.innerHTML = '';
            currentSessionDir = null;
            currentView = 'transcript';
            tabTranscript.classList.add('active');
            tabSummary.classList.remove('active');
            transcriptContent.style.display = '';
            summaryContent.style.display = 'none';
            // 停留在空白页（有"开始录音"按钮），不直接切工作台
            // 会议已在后台创建好，用户点"开始录音"后 startRecording 会直接用这个会议
            voiceEmpty.style.display = '';
            voiceWorkspace.style.display = 'none';
            eventBus.emit('history:refresh');
        } catch (e) {
            showToast('创建会议失败: ' + e.message);
        }
    }
    
    async function loadMeeting(id) {
        try {
            const data = await api(`meetings/${id}`);
            if (!data) {
                showToast('会议不存在');
                return;
            }
            meetingId = id;
            meetingCreated = true;
            voiceTitle.textContent = data.title || '未命名会议';
            transcriptContent.innerHTML = '';
            
            // 加载转写内容
            try {
                const transcriptResp = await api(`meetings/${id}/transcript`);
                if (transcriptResp.chunks && transcriptResp.chunks.length > 0) {
                    transcriptData = transcriptResp.chunks;
                    renderTranscript(transcriptResp.chunks);
                }
            } catch(e) { /* 无转写内容 */ }
            
            // 如果有纪要，渲染纪要
            try {
                await loadSummary();
            } catch(e) { /* 无纪要 */ }
            
            showWorkspace();
        } catch (e) {
            showToast('加载会议失败: ' + e.message);
        }
    }
    
    function showWorkspace() {
        voiceEmpty.style.display = 'none';
        voiceWorkspace.style.display = 'flex';
    }
    
    // ==================== 录音 ====================
    
    async function startRecording() {
        console.log('[Voice] startRecording()');
        if (state.isRecording) return;
        
        const isTauri = typeof window.__TAURI__ !== 'undefined';
        if (!isTauri) {
            showToast('录音功能仅在桌面版可用');
            return;
        }
        
        if (!meetingId || !meetingCreated) {
            try {
                await newMeeting();
            } catch (e) {
                showToast('创建会议失败: ' + e.message);
                return;
            }
        }
        
        // 异步检查 ASR 服务
        try {
            showToast('正在检查 ASR 服务...');
            const asrStatus = await api('asr/status');
            // 兼容两种返回结构：新版 backends[].available，旧版 running
            const backendsOk = Array.isArray(asrStatus?.backends) && asrStatus.backends.some(b => b.available);
            const runningOk = asrStatus?.running === true;
            if (!backendsOk && !runningOk) {
                showToast('ASR 服务未运行，正在启动...');
                await api('asr/ensure_running', {});
                showToast('ASR 服务已就绪');
            } else {
                showToast('ASR 服务就绪');
            }
        } catch (e) {
            showToast('ASR 状态未知，继续录音');
        }
        
        try {
            const invoke = window.__TAURI__.core.invoke;
            const result = await invoke('start_recording', { meetingId: meetingId });
            
            state.isRecording = true;
            recordingStartTime = Date.now();
            
            // UI: 显示工作台 + 录音条
            showWorkspace();
            voiceRecordingBar.style.display = 'flex';
            startRecordingTimer();
            switchView('transcript');
            
            // 监听实时转写事件
            if (window.__TAURI__?.event?.listen) {
                const { listen } = window.__TAURI__.event;
                if (chunkListener) { try { chunkListener(); } catch(e) {} }
                chunkListener = await listen('transcript_chunk', (event) => {
                    const data = event.payload;
                    // 2026-09-11 防幻觉：静音段不入库不入屏，只显示状态提示
                    if (data.silent) {
                        const recStatus = document.getElementById('recStatusText');
                        if (recStatus) recStatus.textContent = `已连续 ${data.silent_streak} 段无声，请检查麦克风权限`;
                        return;
                    }
                    appendTranscriptChunk({
                        text: data.text,
                        start_ts: data.start_ts || 0,
                        speaker: data.speaker || 'unknown',
                        engine: data.engine || 'firered',
                        chunk_id: data.chunk_id,
                    });
                });
                // 2026-09-11 防幻觉：连续静音警告（macOS 麦克风权限被拒 → cpal 采集全零）
                if (micWarningListener) { try { micWarningListener(); } catch(e) {} }
                micWarningListener = await listen('mic_permission_warning', (event) => {
                    const d = event.payload;
                    showToast((d && d.message) || '连续多段无声，请检查麦克风权限');
                });
            }
            
            // 监听标点恢复
            if (window.__TAURI__?.event?.listen) {
                const { listen } = window.__TAURI__.event;
                if (punctuationListener) { try { punctuationListener(); } catch(e) {} }
                punctuationListener = await listen('meeting_progress_' + meetingId, (event) => {
                    const data = event.payload;
                    if (data.type === 'punctuation_done' && data.chunk_id) {
                        const chunkEl = document.querySelector(`[data-chunk-id="${data.chunk_id}"] .chunk-text`);
                        if (chunkEl && data.punctuated_text) {
                            chunkEl.textContent = data.punctuated_text;
                        }
                    }
                });
            }
            
            showToast('录音开始，说话时文字将实时显示');
            
        } catch (e) {
            console.error('[Voice] start_recording 失败:', e);
            showToast('启动录音失败: ' + e);
        }
    }
    
    async function stopRecording() {
        if (!state.isRecording) return;
        state.isRecording = false;
        stopRecordingTimer();
        
        voiceRecordingBar.style.display = 'none';
        
        const isTauri = typeof window.__TAURI__ !== 'undefined';
        if (!isTauri) return;
        
        try {
            const invoke = window.__TAURI__.core.invoke;
            showToast('录音结束，正在转写最后几段...');
            const result = await invoke('stop_recording');
            
            if (chunkListener) { try { chunkListener(); } catch(e) {} chunkListener = null; }
            if (punctuationListener) { try { punctuationListener(); } catch(e) {} punctuationListener = null; }
            
            if (result.segment_count > 0) {
                // 粗转完成，跳过精转/说话人分离，直接生成纪要
                eventBus.emit('history:refresh');

                if (result.session_dir) {
                    currentSessionDir = result.session_dir;
                    // 直接走纪要，不再精转+diarize
                    await generateSummary();
                }
            } else {
                showToast('录音文件为空，请检查麦克风权限');
            }
            
        } catch (e) {
            showToast('停止录音失败: ' + e);
        }
    }
    
    async function handleFileUpload(e) {
        const file = e.target.files[0];
        if (!file) return;
        
        if (!meetingId) { await newMeeting(); }
        
        const isTauri = typeof window.__TAURI__ !== 'undefined';
        
        if (isTauri) {
            try {
                const arrayBuffer = await file.arrayBuffer();
                const uint8 = new Uint8Array(arrayBuffer);
                const tempDir = '/tmp/bijian_audio';
                const tempPath = `${tempDir}/upload_${Date.now()}_${file.name}`;
                
                const fs = window.__TAURI__?.fs || {};
                try { await fs.mkdir(tempDir, { recursive: true }); } catch(e) {}
                await fs.writeFile(tempPath, Array.from(uint8));
                
                const invoke = window.__TAURI__.core.invoke;
                const data = await invoke('upload_audio', { meetingId: meetingId, filePath: tempPath });
                
                if (data.text) {
                    appendTranscriptChunk({
                        text: data.text, start_ts: 0, speaker: 'unknown',
                        engine: data.engine || 'firered',
                    });
                    showToast('文件上传成功，转写完成');
                } else {
                    showToast('转写结果为空');
                }
            } catch (err) {
                showToast('上传失败: ' + err);
            }
        }
        
        e.target.value = '';
    }
    
    // ==================== 最终精转（v3 单引擎 Qwen3-ASR）+ 说话人分离 ====================
    
    async function runCrossValidateThenDiarize() {
        if (!meetingId || !currentSessionDir) return;
        const invoke = window.__TAURI__.core.invoke;
        try {
            showToast('Qwen3-ASR 降噪+全量精转中...');
            
            const cvResult = await invoke('run_cross_validate_transcribe', {
                meetingId: meetingId, sessionDir: currentSessionDir
            });
            console.log('[Voice] 精转返回:', cvResult);
            
            if (cvResult.success) {
                showToast('精转完成: ' + cvResult.final_segments + ' 段，正在分析说话人...');
                eventBus.emit('history:refresh');
                
                // 加载并渲染精转后的最终转写
                try {
                    const transcriptResp = await api(`meetings/${meetingId}/transcript`);
                    if (transcriptResp.chunks && transcriptResp.chunks.length > 0) {
                        transcriptData = transcriptResp.chunks;
                        renderTranscript(transcriptResp.chunks);
                    }
                } catch(e) { console.error('[Voice] 重新加载转写失败:', e); }
                
                await runDiarization();
            } else {
                showToast('精转未生效，直接说话人分离');
                await runDiarization();
            }
        } catch (e) {
            console.error('[Voice] 精转失败:', e);
            showToast('精转失败: ' + e + '，直接说话人分离');
            await runDiarization();
        }
    }
    
    async function runDiarization() {
        if (!meetingId || !currentSessionDir) return;
        const invoke = window.__TAURI__.core.invoke;
        try {
            showToast('正在分析说话人...');
            
            if (window.__TAURI__?.event?.listen) {
                const { listen } = window.__TAURI__.event;
                if (speakersListener) { try { speakersListener(); } catch(e) {} }
                speakersListener = await listen('speakers_assigned', async (event) => {
                    const data = event.payload;
                    if (data.meeting_id === meetingId && data.assigned_chunks > 0) {
                        showToast('说话人分离完成: ' + data.num_speakers + ' 位说话人');
                        try {
                            const transcriptResp = await api(`meetings/${meetingId}/transcript`);
                            if (transcriptResp.chunks && transcriptResp.chunks.length > 0) {
                                transcriptData = transcriptResp.chunks;
                                renderTranscript(transcriptResp.chunks);
                            }
                        } catch(e) { console.error('[Voice] 重新加载转写失败:', e); }
                    } else if (data.assigned_chunks === 0) {
                        showToast('未检测到多个说话人');
                    }
                });
            }
            
            const result = await invoke('run_speaker_diarization', {
                meetingId: meetingId, sessionDir: currentSessionDir
            });
            
            if (!result.success) {
                showToast('说话人分离失败: ' + (result.message || ''));
            }
        } catch (e) {
            console.error('[Voice] 说话人分离失败:', e);
            showToast('说话人分离失败: ' + e);
        }
    }
    
    // ==================== 录音计时 ====================
    function startRecordingTimer() {
        recordingTimer = setInterval(() => {
            const elapsed = Math.floor((Date.now() - recordingStartTime) / 1000);
            const mins = Math.floor(elapsed / 60).toString().padStart(2, '0');
            const secs = (elapsed % 60).toString().padStart(2, '0');
            const timeStr = `${mins}:${secs}`;
            recordingDuration.textContent = timeStr;
        }, 1000);
    }
    
    function stopRecordingTimer() {
        if (recordingTimer) {
            clearInterval(recordingTimer);
            recordingTimer = null;
        }
    }
    
    // ==================== 转写渲染（主框）====================
    let currentSpeaker = null;
    let currentSpeakerEl = null;
    let currentSpeakerTextEl = null;
    let lastSegmentTime = 0; // 用于5分钟分段标记
    
    /**
     * 全量渲染转写：按5分钟时间段分段，段间插入时间标记
     */
    function renderTranscript(chunks) {
        transcriptContent.innerHTML = '';
        currentSpeaker = null;
        currentSpeakerEl = null;
        currentSpeakerTextEl = null;
        lastSegmentTime = -1;
        
        if (!chunks || chunks.length === 0) return;
        
        // 只渲染快速转写段（conf<0.95）；精转段（conf>=0.95）只进下方精转面板
        chunks.filter(c => (c.confidence || 0) < 0.95).forEach(c => {
            const text = c.text || c.transcript || '';
            const speaker = c.speaker || 'unknown';
            if (!text.trim()) return;
            
            const startTime = c.start_time !== undefined ? c.start_time : (c.start_ts || 0);
            
            // 5分钟分段标记
            const segmentIdx = Math.floor(startTime / 300);
            if (segmentIdx !== lastSegmentTime) {
                lastSegmentTime = segmentIdx;
                const marker = document.createElement('div');
                marker.className = 'time-segment-marker';
                const mins = segmentIdx * 5;
                marker.textContent = `--- ${mins}分钟 ---`;
                transcriptContent.appendChild(marker);
                // 新时间段重置说话人
                currentSpeaker = null;
                currentSpeakerEl = null;
                currentSpeakerTextEl = null;
            }
            
            appendTranscriptChunk({ text, speaker, chunk_id: c.id });
        });
    }
    
    /**
     * 追加转写片段（说话人气泡）
     */
    function appendTranscriptChunk(data) {
        const text = (data.text || '').trim();
        if (!text) return;
        
        const speaker = data.speaker || 'unknown';
        const chunkId = data.chunk_id || '';
        
        const isSameSpeaker = speaker === currentSpeaker && currentSpeakerEl && currentSpeakerEl.isConnected;
        
        if (isSameSpeaker) {
            const prevText = currentSpeakerTextEl.textContent.trim();
            const lastChar = prevText.slice(-1);
            const hasPunct = /[。，；！？、,.!?;:]/.test(lastChar);
            const newText = prevText ? (hasPunct ? prevText + text : prevText + ' ' + text) : text;
            currentSpeakerTextEl.textContent = newText;
            transcriptContent.scrollTop = transcriptContent.scrollHeight;
            return;
        }
        
        currentSpeaker = speaker;
        const chunk = document.createElement('div');
        chunk.className = 'transcript-chunk';
        chunk.dataset.chunkId = chunkId;
        chunk.dataset.speaker = speaker;
        
        const speakerDisplay = speaker === 'unknown' ? '说话人' : '说话人 ' + escapeHtml(speaker);
        
        chunk.innerHTML = `
            <div class="chunk-meta">
                <span class="chunk-speaker" title="点击重命名">${speakerDisplay}</span>
                <span class="chunk-edit-btn" title="编辑文本">✏️</span>
            </div>
            <div class="chunk-text" contenteditable="false" spellcheck="false">${escapeHtml(text)}</div>
        `;
        transcriptContent.appendChild(chunk);
        transcriptContent.scrollTop = transcriptContent.scrollHeight;
        
        currentSpeakerEl = chunk;
        currentSpeakerTextEl = chunk.querySelector('.chunk-text');
        
        // 说话人重命名
        const speakerEl = chunk.querySelector('.chunk-speaker');
        speakerEl.addEventListener('click', () => {
            if (speakerEl.contentEditable === 'true') return;
            speakerEl.contentEditable = 'true';
            speakerEl.focus();
            document.execCommand('selectAll', false, null);
        });
        
        speakerEl.addEventListener('blur', async () => {
            if (speakerEl.contentEditable !== 'true') return;
            const newLabel = speakerEl.textContent.trim();
            speakerEl.contentEditable = 'false';
            if (!newLabel || newLabel === speakerDisplay) return;
            
            const invoke = window.__TAURI__?.core?.invoke;
            const origSpeaker = chunk.dataset.speaker;
            if (invoke && origSpeaker && origSpeaker !== 'unknown') {
                try {
                    await invoke('rename_speaker', {
                        meetingId: meetingId, speakerId: origSpeaker, label: newLabel
                    });
                    showToast('已重命名');
                    document.querySelectorAll(`.transcript-chunk[data-speaker="${origSpeaker}"] .chunk-speaker`).forEach(el => {
                        el.textContent = newLabel;
                    });
                } catch(e) {
                    showToast('重命名失败: ' + e);
                    speakerEl.textContent = speakerDisplay;
                }
            } else {
                document.querySelectorAll(`.transcript-chunk[data-speaker="${origSpeaker}"] .chunk-speaker`).forEach(el => {
                    el.textContent = newLabel;
                });
            }
        });
        
        speakerEl.addEventListener('keydown', (e) => {
            if (e.key === 'Enter') { e.preventDefault(); speakerEl.blur(); }
        });
        
        // 文本编辑
        const editBtn = chunk.querySelector('.chunk-edit-btn');
        const textEl = chunk.querySelector('.chunk-text');
        const chunkIdLocal = chunkId;
        let originalText = textEl.textContent;
        
        editBtn.addEventListener('click', () => {
            textEl.contentEditable = 'true';
            textEl.focus();
            editBtn.style.display = 'none';
        });
        
        textEl.addEventListener('blur', async () => {
            if (textEl.contentEditable !== 'true') return;
            const newText = textEl.textContent.trim();
            textEl.contentEditable = 'false';
            editBtn.style.display = 'inline';
            if (!newText || newText === originalText) return;
            try {
                await api(`meetings/${meetingId}/transcript/${chunkIdLocal}`, { text: newText }, 'PATCH');
                originalText = newText;
                showToast('已保存');
            } catch (e) {
                showToast('保存失败: ' + e.message);
            }
        });
    }
    
    function formatTime(seconds) {
        const mins = Math.floor(seconds / 60);
        const secs = Math.floor(seconds % 60);
        return `${mins.toString().padStart(2, '0')}:${secs.toString().padStart(2, '0')}`;
    }
    
    // ==================== AI 纠错 ====================
    async function startCorrection() {
        if (!meetingId) return;

        const btn = aiCorrectBtn;
        if (btn) { btn.disabled = true; btn.style.opacity = '0.5'; }
        showToast('开始 AI 纠错...');
        showProgress('正在准备纠错...', 0);

        try {
            const invoke = window.__TAURI__?.core?.invoke;
            if (!invoke) {
                showToast('AI 纠错需要桌面端环境');
                return;
            }

            await invoke('trigger_correction', { meetingId });

            const { listen } = window.__TAURI__.event;

            // 监听纠错进度
            let progressListener;
            if (window.__TAURI__?.event?.listen) {
                progressListener = await listen('correction_progress', (event) => {
                    const data = event.payload;
                    if (data.meeting_id !== meetingId) return;
                    if (data.phase === 'start') {
                        showProgress(`开始纠错 ${data.total} 段文本...`, 5);
                    } else if (data.phase === 'processing') {
                        const pct = Math.round((data.current / data.total) * 90);
                        showProgress(`纠错中 ${data.current}/${data.total} 段`, pct);
                    } else if (data.phase === 'done') {
                        showProgress(`纠错完成`, 100);
                        setTimeout(() => {
                            hideProgress();
                            loadMeeting(meetingId);
                            showToast(`纠错完成，修正 ${data.corrected}/${data.total} 段`);
                        }, 500);
                        if (progressListener) progressListener();
                    }
                });
            }

            // 同时监听 meeting_progress 事件
            const eventName = `meeting_progress_${meetingId}`;
            let meetingListener;
            meetingListener = await listen(eventName, (event) => {
                const data = event.payload;
                if (data.type === 'correction_done') {
                    hideProgress();
                    loadMeeting(meetingId);
                    showToast(`纠错完成，修正 ${data.corrected} 段`);
                    if (meetingListener) meetingListener();
                    if (progressListener) progressListener();
                    if (btn) { btn.disabled = false; btn.style.opacity = '1'; }
                }
            });

            // 兜底超时（5 分钟）
            setTimeout(() => {
                hideProgress();
                if (btn) { btn.disabled = false; btn.style.opacity = '1'; }
                if (progressListener) progressListener();
                if (meetingListener) meetingListener();
            }, 300000);

        } catch (e) {
            hideProgress();
            showToast('纠错失败: ' + (e.message || e));
        } finally {
            if (btn) { btn.disabled = false; btn.style.opacity = '1'; }
        }
    }

    // ==================== 纪要 ====================
    async function generateSummary() {
        if (!meetingId) return;
        
        const templateType = meetingTemplateSelect ? meetingTemplateSelect.value : 'meeting';
        const depth = summaryDepthSelect ? summaryDepthSelect.value : 'standard';
        
        try {
            showToast('正在生成纪要...');
            showProgress('开始生成纪要...', 0);
            await api(`meetings/${meetingId}/aggregate`, { template_type: templateType, depth: depth });
            
            const isTauri = typeof window.__TAURI__ !== 'undefined';
            if (isTauri) {
                const eventName = `meeting_progress_${meetingId}`;
                const { listen } = window.__TAURI__.event;
                listen(eventName, (event) => {
                    const data = event.payload;
                    if (data.type === 'final_summary_ready') {
                        hideProgress();
                        loadSummary();
                    } else if (data.type === 'final_summary_failed') {
                        hideProgress();
                        clearProgressQueue();
                        showToast('纪要生成失败：' + (data.reason || '无有效转写内容'));
                    }
                });

                // 监听纪要进度事件
                if (window.__TAURI__?.event?.listen) {
                    listen('summary_progress', (event) => {
                        const data = event.payload;
                        if (data.meeting_id !== meetingId) return;
                        if (data.phase === 'start') {
                            showProgress('正在分析转写内容...', 5);
                        } else if (data.phase === 'mapping') {
                            const pct = Math.round(10 + (data.current / data.total) * 70);
                            showProgress(`提取要点 ${data.current}/${data.total} 片`, pct);
                            renderProgressQueue(data.current, data.total);
                        } else if (data.phase === 'reducing') {
                            showProgress('汇总生成纪要中...', 85);
                            renderProgressQueue(data.total || 0, data.total || 0, true);
                        } else if (data.phase === 'done') {
                            showProgress('纪要生成完成', 100);
                            setTimeout(() => { hideProgress(); clearProgressQueue(); }, 1200);
                        }
                    });
                }
            }
            
            let attempts = 0;
            const poll = setInterval(async () => {
                attempts++;
                if (attempts > 60) {
                    clearInterval(poll);
                    hideProgress();
                    showToast('纪要生成超时');
                    return;
                }
                try {
                    const data = await api(`meetings/${meetingId}/summary`);
                    if (data) {
                        clearInterval(poll);
                        hideProgress();
                        renderSummary(data);
                        switchView('summary');
                    }
                } catch(e) {}
            }, 2000);
            
        } catch (e) {
            hideProgress();
            showToast('生成失败: ' + e.message);
        }
    }
    
    async function loadSummary() {
        try {
            const data = await api(`meetings/${meetingId}/summary`);
            renderSummary(data);
        } catch (e) { /* 纪要尚未生成 */ }
    }
    
    function renderSummary(data) {
        let html = '';
        
        if (data.record) {
            html += `<div class="summary-tldr"><strong>${escapeHtml(data.tldr || '记录式纪要')}</strong></div>`;
            
            if (data.content) {
                html += '<div class="summary-record">';
                data.content.split('\n').forEach(line => {
                    line = line.trim();
                    if (line) {
                        html += `<div class="record-line">${escapeHtml(line)}</div>`;
                    }
                });
                html += '</div>';
            } else if (data.lines) {
                html += '<div class="summary-record">';
                data.lines.forEach(line => {
                    html += `<div class="record-line">${escapeHtml(line)}</div>`;
                });
                html += '</div>';
            }
            
            summaryContent.innerHTML = html;
            return;
        }
        
        if (data.tldr) {
            html += `<div class="summary-tldr"><strong>概要：</strong>${escapeHtml(data.tldr)}</div>`;
        }
        
        // 核心结论（key_points）— brief 和 standard 版有
        if (data.key_points && data.key_points.length > 0) {
            html += '<div class="summary-section"><h4>核心结论</h4><ul>';
            data.key_points.forEach(kp => {
                html += `<li>${escapeHtml(kp)}</li>`;
            });
            html += '</ul></div>';
        }
        
        // 主题/议题
        if (data.topics && data.topics.length > 0) {
            html += '<div class="summary-section"><h4>议题讨论</h4>';
            data.topics.forEach(t => {
                html += `<div class="summary-topic">`;
                html += `<div class="summary-topic-title">${escapeHtml(t.title || '')}</div>`;
                if (t.summary) {
                    html += `<div class="summary-topic-summary">${escapeHtml(t.summary)}</div>`;
                }
                // 详细版：key_data
                if (t.key_data && t.key_data.length > 0) {
                    html += `<div class="summary-key-data">`;
                    t.key_data.forEach(d => {
                        html += `<span class="key-data-tag">${escapeHtml(d)}</span>`;
                    });
                    html += `</div>`;
                }
                html += `</div>`;
            });
            html += '</div>';
        }
        
        // 决策
        if (data.decisions && data.decisions.length > 0) {
            html += '<div class="summary-section"><h4>决策</h4><ul>';
            data.decisions.forEach(d => {
                if (typeof d === 'string') {
                    html += `<li>${escapeHtml(d)}</li>`;
                } else {
                    html += `<li>${escapeHtml(d.text || '')}`;
                    if (d.context) {
                        html += `<span class="decision-context">${escapeHtml(d.context)}</span>`;
                    }
                    html += `</li>`;
                }
            });
            html += '</ul></div>';
        }
        
        // 行动项
        if (data.actions && data.actions.length > 0) {
            html += '<div class="summary-section"><h4>行动项</h4><ul>';
            data.actions.forEach(a => {
                const text = typeof a === 'string' ? a : a.text;
                const assignee = a.assignee ? ` — ${escapeHtml(a.assignee)}` : '';
                const deadline = a.deadline ? ` <span class="action-deadline">截止: ${escapeHtml(a.deadline)}</span>` : '';
                html += `<li>${escapeHtml(text)}${assignee}${deadline}</li>`;
            });
            html += '</ul></div>';
        }
        
        // 待解决问题
        if (data.questions && data.questions.length > 0) {
            html += '<div class="summary-section"><h4>待解决问题</h4><ul>';
            data.questions.forEach(q => {
                html += `<li>${escapeHtml(q.text || q)}</li>`;
            });
            html += '</ul></div>';
        }
        
        // 详细版：不确定信息
        if (data.uncertainties && data.uncertainties.length > 0) {
            html += '<div class="summary-section"><h4>待确认信息</h4><ul class="uncertainty-list">';
            data.uncertainties.forEach(u => {
                html += `<li>${escapeHtml(u)}</li>`;
            });
            html += '</ul></div>';
        }
        
        // 详细版：说话人要点
        if (data.speaker_notes && data.speaker_notes.length > 0) {
            html += '<div class="summary-section"><h4>各说话人要点</h4>';
            data.speaker_notes.forEach(sn => {
                html += `<div class="speaker-note">`;
                html += `<div class="speaker-note-name">${escapeHtml(sn.speaker || '说话人')}</div>`;
                html += `<ul>`;
                (sn.key_points || []).forEach(kp => {
                    html += `<li>${escapeHtml(kp)}</li>`;
                });
                html += `</ul>`;
                html += `</div>`;
            });
            html += '</div>';
        }
        
        if (data.markdown) {
            html += `<div class="summary-markdown"><pre>${escapeHtml(data.markdown)}</pre></div>`;
        }
        
        summaryContent.innerHTML = html || '<p>无内容</p>';
    }
    
    // ==================== 保存为笔记 ====================
    async function saveAsNote() {
        if (!meetingId) { showToast('没有活跃会议'); return; }
        
        try {
            const transcriptResp = await api(`meetings/${meetingId}/transcript`);
            const chunks = transcriptResp.chunks || [];
            
            if (chunks.length === 0) {
                showToast('没有可保存的转写内容');
                return;
            }
            
            let summaryData = {};
            try { summaryData = await api(`meetings/${meetingId}/summary`); } catch(e) {}
            
            let content = `# ${voiceTitle.textContent}\n\n`;
            content += `> 创建时间: ${new Date().toLocaleString('zh-CN')}\n\n`;
            
            if (summaryData.tldr) {
                content += `## 概要\n${summaryData.tldr}\n\n`;
            }
            
            content += `## 转写内容\n\n`;
            chunks.forEach(c => {
                content += `[${formatTime(c.start_time || c.start_ts || 0)}] ${c.speaker || 'unknown'}: ${c.transcript || c.text || ''}\n\n`;
            });
            
            if (summaryData.topics && summaryData.topics.length > 0) {
                content += `## 主题\n`;
                summaryData.topics.forEach(t => {
                    content += `- **${t.title || ''}**: ${t.summary || ''}\n`;
                });
                content += '\n';
            }
            
            if (summaryData.actions && summaryData.actions.length > 0) {
                content += `## 行动项\n`;
                summaryData.actions.forEach(a => {
                    const text = typeof a === 'string' ? a : a.text;
                    const assignee = a.assignee ? ` (${a.assignee})` : '';
                    content += `- ${text}${assignee}\n`;
                });
                content += '\n';
            }
            
            await api('notes', {
                title: voiceTitle.textContent,
                content: content,
                tags: '会议,语音转写'
            });
            
            showToast('已保存为笔记');
            eventBus.emit('notes:refresh');
            
        } catch (e) {
            showToast('保存失败: ' + e.message);
        }
    }
    
    // ==================== 工具 ====================
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

    // ==================== 进度条 ====================
    function showProgress(text, pct) {
        if (!progressBarContainer) return;
        progressBarContainer.style.display = 'block';
        if (text) progressBarInfo.textContent = text;
        if (pct !== undefined) progressBarFill.style.width = pct + '%';
    }

    function hideProgress() {
        if (!progressBarContainer) return;
        progressBarContainer.style.display = 'none';
        progressBarFill.style.width = '0%';
        if (progressQueue) { progressQueue.innerHTML = ''; progressQueue.style.display = 'none'; }
    }

    // 分片队列渲染（类下载进度）：每片一个色块，pending/active/done 三态
    function renderProgressQueue(current, total, allDone) {
        if (!progressQueue) return;
        if (!total || total <= 0) { progressQueue.style.display = 'none'; return; }
        progressQueue.style.display = 'flex';
        let html = '<span class="progress-queue-label">分片</span>';
        for (let i = 1; i <= total; i++) {
            let cls = 'pending';
            if (allDone || i <= current) cls = 'done';
            else if (i === current + 1) cls = 'active';
            html += `<span class="progress-queue-item ${cls}"></span>`;
        }
        progressQueue.innerHTML = html;
    }

    function clearProgressQueue() {
        if (!progressQueue) return;
        progressQueue.innerHTML = '';
        progressQueue.style.display = 'none';
    }

    // ==================== 录音播放 ====================
    let audioPlayer = null;

    async function togglePlayback() {
        // 已在播放 → 暂停
        if (audioPlayer && !audioPlayer.paused) {
            audioPlayer.pause();
            updatePlayBtn(false);
            return;
        }
        // 已暂停 → 继续
        if (audioPlayer && audioPlayer.paused && audioPlayer.currentTime > 0) {
            audioPlayer.play();
            updatePlayBtn(true);
            return;
        }

        // 首次播放 → 获取录音文件路径
        if (!meetingId) { showToast('没有活跃会议'); return; }
        const isTauri = typeof window.__TAURI__ !== 'undefined';
        if (!isTauri || !window.__TAURI__?.core?.invoke) {
            showToast('播放功能仅在桌面版可用');
            return;
        }
        try {
            showToast('正在加载录音...');
            const invoke = window.__TAURI__.core.invoke;
            const result = await invoke('get_meeting_audio', { meetingId });
            if (!result.exists || !result.audio_path) {
                showToast('未找到录音文件');
                return;
            }

            // Tauri v2: 用 convertFileSrc 把本地路径转为 WebView 可访问的 asset URL
            let audioUrl;
            if (window.__TAURI__?.core?.convertFileSrc) {
                audioUrl = window.__TAURI__.core.convertFileSrc(result.audio_path);
            } else {
                audioUrl = 'file://' + result.audio_path;
            }

            audioPlayer = new Audio(audioUrl);
            audioPlayer.addEventListener('ended', () => {
                updatePlayBtn(false);
                showToast('播放结束');
            });
            audioPlayer.addEventListener('error', () => {
                updatePlayBtn(false);
                showToast('播放失败：无法加载录音文件');
            });
            await audioPlayer.play();
            updatePlayBtn(true);
        } catch (e) {
            updatePlayBtn(false);
            showToast('播放失败: ' + (e.message || e));
        }
    }

    function updatePlayBtn(playing) {
        if (!playAudioBtn) return;
        playAudioBtn.classList.toggle('playing', playing);
        const label = playAudioBtn.querySelector('svg');
        if (playing) {
            // 暂停图标
            label.innerHTML = '<rect x="6" y="4" width="4" height="16"/><rect x="14" y="4" width="4" height="16"/>';
        } else {
            // 播放图标
            label.innerHTML = '<polygon points="5 3 19 12 5 21 5 3"/>';
        }
    }

    // ==================== 精转进度监听 ====================
    function bindRefineProgressListener() {
        if (typeof window.__TAURI__ === 'undefined') return;
        if (!window.__TAURI__?.event?.listen) return;
        const { listen } = window.__TAURI__.event;
        listen('refine_progress', (event) => {
            const data = event.payload;
            if (data.meeting_id !== meetingId) return;
            if (data.phase === 'start') {
                showProgress(`开始精转，共 ${data.total_windows} 片`, 5);
            } else if (data.phase === 'processing') {
                const pct = Math.round((data.current / data.total) * 90);
                const eta = data.elapsed_secs > 0 && data.current > 0
                    ? Math.round((data.total - data.current) * (data.elapsed_secs / data.current))
                    : 0;
                const etaStr = eta > 0 ? `（预计还需 ${Math.round(eta/60)} 分钟）` : '';
                showProgress(`精转中 ${data.current}/${data.total} 片${etaStr}`, pct);
            } else if (data.phase === 'done') {
                showProgress('精转完成', 100);
                setTimeout(() => hideProgress(), 1500);
            }
        });
    }

    // ==================== 启动 ====================
    document.addEventListener('DOMContentLoaded', init);
})();
