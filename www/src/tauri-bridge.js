/**
 * PanNote - Tauri IPC 桥接层
 * 通过拦截 fetch 实现前端代码零修改
 */
(function() {
    'use strict';
    
    const isTauri = typeof window.__TAURI__ !== 'undefined';
    if (!isTauri) return;
    
    const invoke = window.__TAURI__.core.invoke;
    const originalFetch = window.fetch;
    
    window.fetch = async function(input, init) {
        const url = typeof input === 'string' ? input : input?.url || '';
        const method = (init?.method || 'GET').toUpperCase();
        const isPost = method === 'POST';
        const isDelete = method === 'DELETE';
        const isPatch = method === 'PATCH';
        
        // FormData 请求特殊处理（音频上传）
        const isFormData = init?.body instanceof FormData;
        if (isFormData && url.includes('/api/meetings/') && url.includes('/upload_audio')) {
            const mid = url.match(/meetings\/([^/]+)\/upload_audio/)?.[1] || '';
            const file = init.body.get('file');
            if (!file) return { ok: false, status: 400, json: async () => ({ error: 'no file' }) };
            
            // 把 Blob 写入临时文件，再传路径
            const arrayBuffer = await file.arrayBuffer();
            const uint8 = new Uint8Array(arrayBuffer);
            // 用 Tauri 写入临时文件
            const { writeFile, readFile } = window.__TAURI__?.fs || {};
            // 临时音频目录用应用私有路径（不再用 /tmp，隐私安全）
            const home = await window.__TAURI__?.os?.homedir?.() || '';
            const tempDir = home ? `${home}/Library/Application Support/PanNote/tmp_audio` : '/tmp/bijian_audio';
            const tempPath = `${tempDir}/${Date.now()}.wav`;
            
            try {
                // 确保目录存在
                const { mkdir } = window.__TAURI__?.fs || {};
                try { await mkdir(tempDir, { recursive: true }); } catch(e) {}
                
                // 写文件
                await writeFile(tempPath, Array.from(uint8));
                
                const result = await invoke('upload_audio', { meetingId: mid, filePath: tempPath });
                return { ok: true, status: 200, json: async () => result, text: async () => JSON.stringify(result) };
            } catch (e) {
                return { ok: false, status: 500, json: async () => ({ error: e.toString() }) };
            }
        }
        
        const apiMatch = url.match(/^\/api\/([^?]+)/);
        if (!apiMatch) return originalFetch(input, init);
        
        const command = apiMatch[1];
        const body = isPost || isPatch ? JSON.parse(init?.body || '{}') : {};
        
        // 路径匹配
        const sessionMsgMatch = command.match(/^chat\/sessions\/([^/]+)\/messages$/);
        // 兼容 stream_message 的路径变体
        const sessionStreamMatch = command.match(/^chat\/sessions\/([^/]+)\/messages$/);
        const sessionMatch = command.match(/^chat\/sessions\/([^/]+)$/);
        const messageMatch = command.match(/^chat\/messages\/([^/]+)\/rate$/);
        const noteMatch = command.match(/^notes\/([^/]+)$/);
        const meetingTranscriptMatch = command.match(/^meetings\/([^/]+)\/transcript\/([^/]+)$/);
        const meetingMatch = command.match(/^meetings\/([^/]+)(\/.*)?$/);
        
        try {
            let result;
            
            // ========== 健康检查 ==========
            if (command === 'health') {
                result = await invoke('health_check');
            }
            
            // ========== AI 对话 ==========
            else if (command === 'chat/create' && isPost) {
                result = await invoke('create_chat_session', { title: body.title, role: body.role });
            } else if (command === 'chat/sessions' && !isPost) {
                result = { sessions: await invoke('list_chat_sessions') };
            } else if (sessionMsgMatch && isPost) {
                // 流式发送消息 - 调用 stream_message Tauri command
                // 后台任务通过 Tauri event 推送流式内容
                const sid = sessionMsgMatch[1];
                const aiResult = await invoke('stream_message', {
                    sessionId: sid, message: body.message, model: body.model, enableTools: body.enable_tools
                });
                // 返回特殊的 Response，告诉前端去监听 Tauri event
                return {
                    ok: true,
                    status: 200,
                    json: async () => aiResult,
                    text: async () => JSON.stringify(aiResult),
                    // 标记：前端需要通过 Tauri event 监听流式内容
                    _isStreamInit: true,
                    _sessionId: sid
                };
            } else if (sessionMatch && !isPost && !isPatch) {
                result = await invoke('get_chat_session', { sessionId: sessionMatch[1] });
            } else if (sessionMatch && isPatch) {
                result = await invoke('update_chat_title', { sessionId: sessionMatch[1], title: body.title });
            } else if (messageMatch && isPost) {
                result = await invoke('rate_message', { messageId: messageMatch[1], rating: body.rating });
            } else if (sessionMatch && isDelete) {
                result = await invoke('delete_chat_session', { sessionId: sessionMatch[1] });
            }
            
            // ========== 进化系统 ==========
            else if (command === 'evolution/distill' && isPost) {
                result = { message: await invoke('distill_evolution') };
            } else if (command === 'evolution/insights') {
                result = { insights: await invoke('get_evolution_insights') };
            }
            
            // ========== 笔记管理 ==========
            else if (command === 'notes' && isPost) {
                result = await invoke('create_note', { title: body.title, content: body.content, tags: body.tags });
            } else if (command === 'notes' && !isPost) {
                result = { notes: await invoke('list_notes') };
            } else if (noteMatch && !isPost && !isDelete && !isPatch) {
                result = await invoke('get_note', { noteId: noteMatch[1] });
            } else if (noteMatch && isPost) {
                result = await invoke('update_note', { noteId: noteMatch[1], title: body.title, content: body.content, tags: body.tags });
            } else if (noteMatch && isDelete) {
                result = await invoke('delete_note', { noteId: noteMatch[1] });
            }
            
            // ========== 搜索 ==========
            else if (command === 'search') {
                const urlParams = new URLSearchParams(url.split('?')[1] || '');
                result = { results: await invoke('search', { query: urlParams.get('q'), limit: parseInt(urlParams.get('limit')) || 20 }) };
            }
            
            // ========== 会议管理 ==========
            else if (command === 'meetings/create' && isPost) {
                result = await invoke('create_meeting', { title: body.title, noteId: body.note_id });
            } else if (command === 'meetings' && !isPost) {
                result = { meetings: await invoke('list_meetings') };
            } else if (command === 'meetings/by_note' && !isPost) {
                const urlParams = new URLSearchParams(url.split('?')[1] || '');
                result = { meetings: await invoke('list_meetings_by_note', { noteId: urlParams.get('note_id') || body.note_id }) };
            } else if (command === 'meetings/sync_note_title' && isPost) {
                result = await invoke('sync_note_title_to_meetings', { noteId: body.note_id, title: body.title });
            } else if (meetingTranscriptMatch && isPatch) {
                result = await invoke('edit_transcript_chunk', { meetingId: meetingTranscriptMatch[1], chunkId: meetingTranscriptMatch[2], text: body.text });
            } else if (meetingMatch) {
                const mid = meetingMatch[1];
                const sub = meetingMatch[2];
                if (sub === '/upload_audio' && isPost) {
                    result = await invoke('upload_audio', { meetingId: mid, filePath: body.file_path });
                } else if (sub === '/aggregate' && isPost) {
                    result = await invoke('trigger_aggregate', { meetingId: mid, templateType: body.template_type, depth: body.depth, force: body.force });
                } else if (sub === '/transcription_stats' && !isPost) {
                    // v2.5.2 状态卡片数据源：段级分布 + 引擎分布 + fallback 计数
                    result = await invoke('get_transcription_stats', { meetingId: mid });
                } else if (sub === '/retry_failed' && isPost) {
                    // v2.5.2 失败段一键重试
                    result = await invoke('retry_failed_segments', { meetingId: mid });
                } else if (sub === '/summary' && !isPost) {
                    result = await invoke('get_summary', { meetingId: mid });
                } else if (sub === '/transcript' && !isPost) {
                    result = { chunks: await invoke('get_transcript', { meetingId: mid }) };
                } else if (sub === '/title' && isPatch) {
                    result = await invoke('update_meeting_title', { meetingId: mid, title: body.title });
                } else if (sub === '/title/auto' && isPost) {
                    result = await invoke('auto_meeting_title', { meetingId: mid });
                } else if (sub === '/audio' && !isPost) {
                    result = await invoke('get_meeting_audio', { meetingId: mid });
                } else if (!sub && !isPost) {
                    result = await invoke('get_meeting', { meetingId: mid });
                }
            }
            
            // ========== ASR ==========
            else if (command === 'asr/status') {
                result = await invoke('asr_status');
            } else if (command === 'asr/ensure_running' && isPost) {
                result = await invoke('asr_ensure_running');
            }
            
            // ========== 许可证管理 ==========
            else if (command === 'license/activate' && isPost) {
                result = await invoke('activate_license', { key: body.key });
            } else if (command === 'license/status') {
                result = await invoke('get_license_status');
            } else if (command === 'license/deactivate' && isPost) {
                result = await invoke('deactivate_license');
            }
            
            // ========== 功能门控 ==========
            else if (command === 'feature/check') {
                const urlParams = new URLSearchParams(url.split('?')[1] || '');
                result = await invoke('check_feature', { feature: urlParams.get('f') || body.feature });
            }
            
            else {
                return originalFetch(input, init);
            }
            
            return {
                ok: true,
                status: 200,
                json: async () => result,
                text: async () => JSON.stringify(result)
            };
        } catch (e) {
            return {
                ok: false,
                status: 500,
                json: async () => ({ error: e.toString() }),
                text: async () => e.toString()
            };
        }
    };
})();
