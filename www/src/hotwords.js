/**
 * PanNote - 热词管理模块
 * 添加/删除/导入热词，查看用户纠错自动沉淀的热词
 * 后端命令：add_hotword / list_hotwords / delete_hotword / import_hotwords
 */
(function() {
    'use strict';

    const { $, eventBus } = window.bijian;
    const invoke = window.__TAURI__?.core?.invoke;

    // ==================== 热词面板 ====================
    async function showPanel() {
        const existing = document.getElementById('hotwordPanel');
        if (existing) existing.remove();

        const overlay = document.createElement('div');
        overlay.id = 'hotwordPanel';
        overlay.style.cssText = 'position:fixed;inset:0;background:rgba(0,0,0,.45);z-index:9999;display:flex;align-items:center;justify-content:center;';

        overlay.innerHTML = `
            <div style="background:#fff;border-radius:16px;padding:24px 28px;max-width:600px;width:90%;box-shadow:0 12px 40px rgba(0,0,0,.2);max-height:85vh;display:flex;flex-direction:column;">
                <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:16px;">
                    <h2 style="font-size:18px;font-weight:600;margin:0;">热词管理</h2>
                    <button id="hwClose" style="border:none;background:transparent;font-size:20px;cursor:pointer;color:#999;padding:0 4px;">✕</button>
                </div>

                <!-- 添加热词 -->
                <div style="display:flex;gap:8px;margin-bottom:12px;">
                    <input id="hwWrong" placeholder="ASR 可能听错的词" style="flex:1;border:1px solid #d1d5db;border-radius:8px;padding:8px 10px;font-size:13px;box-sizing:border-box;">
                    <span style="display:flex;align-items:center;color:#9ca3af;font-size:14px;">→</span>
                    <input id="hwCorrect" placeholder="正确文本" style="flex:1;border:1px solid #d1d5db;border-radius:8px;padding:8px 10px;font-size:13px;box-sizing:border-box;">
                    <button id="hwAdd" style="padding:8px 16px;border:none;border-radius:8px;background:#6366f1;color:#fff;font-size:13px;cursor:pointer;white-space:nowrap;">添加</button>
                </div>

                <!-- 工具栏 -->
                <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:10px;">
                    <div style="display:flex;gap:8px;align-items:center;">
                        <select id="hwSceneFilter" style="border:1px solid #d1d5db;border-radius:6px;padding:4px 8px;font-size:12px;color:#6b7280;">
                            <option value="">全部场景</option>
                            <option value="general">通用</option>
                            <option value="finance">财务</option>
                            <option value="tech">技术</option>
                            <option value="daily">日常</option>
                            <option value="medical">医疗</option>
                            <option value="legal">法律</option>
                        </select>
                        <select id="hwSourceFilter" style="border:1px solid #d1d5db;border-radius:6px;padding:4px 8px;font-size:12px;color:#6b7280;">
                            <option value="">全部来源</option>
                            <option value="manual">手动添加</option>
                            <option value="user_correction">自动纠错</option>
                            <option value="imported">导入</option>
                        </select>
                    </div>
                    <div style="display:flex;gap:6px;">
                        <button id="hwImport" style="padding:5px 10px;border:1px solid #e5e7eb;border-radius:6px;background:transparent;color:#6b7280;font-size:12px;cursor:pointer;">导入</button>
                        <button id="hwExport" style="padding:5px 10px;border:1px solid #e5e7eb;border-radius:6px;background:transparent;color:#6b7280;font-size:12px;cursor:pointer;">导出</button>
                    </div>
                </div>

                <!-- 纠错统计 -->
                <div id="hwStats" style="display:none;margin-bottom:10px;padding:10px 12px;background:#f9fafb;border-radius:8px;border:1px solid #e5e7eb;">
                    <div style="display:flex;gap:16px;align-items:center;margin-bottom:6px;">
                        <span style="font-size:13px;color:#374151;font-weight:500;">纠错样本库</span>
                        <span id="hwStatTotal" style="font-size:18px;font-weight:700;color:#6366f1;">0</span>
                        <span style="font-size:12px;color:#9ca3af;">条样本</span>
                    </div>
                    <div id="hwStatScenes" style="display:flex;gap:6px;flex-wrap:wrap;margin-bottom:6px;"></div>
                    <div id="hwStatTop" style="margin-top:6px;"></div>
                </div>

                <!-- v2.6.0 纠错反哺：存量样本蒸馏成热词建议，一键应用后下次转写生效 -->
                <div id="hwFeedback" style="display:none;margin-bottom:10px;padding:10px 12px;background:#f0f9ff;border-radius:8px;border:1px solid #bae6fd;">
                    <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:6px;">
                        <span style="font-size:13px;color:#0c4a6e;font-weight:500;">纠错反哺建议（历史纠错样本蒸馏，应用后下次转写生效）</span>
                        <button id="hwApplyAll" style="padding:4px 12px;border:none;border-radius:6px;background:#0284c7;color:#fff;font-size:12px;cursor:pointer;">应用全部</button>
                    </div>
                    <div id="hwFeedbackList" style="max-height:110px;overflow-y:auto;"></div>
                </div>

                <!-- v2.6.0 健康报告：不翻日志即可判断转写是否正常 -->
                <div id="hwHealth" style="display:none;margin-bottom:10px;padding:10px 12px;background:#f9fafb;border-radius:8px;border:1px solid #e5e7eb;">
                    <div style="font-size:13px;color:#374151;font-weight:500;margin-bottom:6px;">运行健康</div>
                    <div id="hwHealthBody" style="font-size:12px;color:#6b7280;line-height:1.8;"></div>
                </div>

                <!-- 热词列表 -->
                <div id="hwList" style="flex:1;overflow-y:auto;border:1px solid #f0f0f0;border-radius:10px;padding:4px;min-height:200px;">
                    <div style="text-align:center;color:#b5b9cc;font-size:13px;padding:20px;">加载中…</div>
                </div>

                <!-- 导入文本框（默认隐藏） -->
                <div id="hwImportBox" style="display:none;margin-top:10px;">
                    <textarea id="hwImportText" placeholder="每行一条，格式：错误词,正确词&#10;例如：&#10;网不层,网布层&#10;数政,数智" style="width:100%;height:80px;border:1px solid #d1d5db;border-radius:8px;padding:8px 10px;font-size:12px;font-family:monospace;resize:none;box-sizing:border-box;"></textarea>
                    <button id="hwImportConfirm" style="margin-top:6px;padding:6px 14px;border:none;border-radius:6px;background:#6366f1;color:#fff;font-size:12px;cursor:pointer;">确认导入</button>
                </div>
            </div>
        `;

        document.body.appendChild(overlay);

        // 绑定事件
        overlay.querySelector('#hwClose').addEventListener('click', () => overlay.remove());
        overlay.addEventListener('click', (e) => { if (e.target === overlay) overlay.remove(); });

        overlay.querySelector('#hwAdd').addEventListener('click', handleAdd);
        overlay.querySelector('#hwSceneFilter').addEventListener('change', () => loadList());
        overlay.querySelector('#hwSourceFilter').addEventListener('change', () => loadList());

        overlay.querySelector('#hwImport').addEventListener('click', () => {
            const box = overlay.querySelector('#hwImportBox');
            box.style.display = box.style.display === 'none' ? 'block' : 'none';
        });

        overlay.querySelector('#hwImportConfirm').addEventListener('click', handleImport);

        // v2.6.0 纠错反哺：应用全部建议
        const applyAllBtn = overlay.querySelector('#hwApplyAll');
        if (applyAllBtn) applyAllBtn.addEventListener('click', async () => {
            const listEl = overlay.querySelector('#hwFeedbackList');
            const suggestions = (listEl && listEl._suggestions) || [];
            if (!suggestions.length) { showToast('没有可应用的建议'); return; }
            try {
                const r = await invoke('apply_correction_hotwords', { suggestions });
                showToast(`已应用 ${r.applied} 条热词（跳过 ${r.skipped} 条），下次转写生效`);
                loadFeedback(); loadList(); loadStats();
            } catch (e) { showToast('应用失败：' + e); }
        });

        overlay.querySelector('#hwExport').addEventListener('click', handleExport);

        // Enter 键添加
        overlay.querySelector('#hwCorrect').addEventListener('keydown', (e) => {
            if (e.key === 'Enter') handleAdd();
        });

        // 初始加载
        await loadList();
        loadStats(); // 异步加载统计，不阻塞列表
        loadFeedback(); // v2.6.0 纠错反哺建议
        loadHealth();  // v2.6.0 健康报告
    }

    // ==================== 加载热词列表 ====================
    async function loadList() {
        const overlay = document.getElementById('hotwordPanel');
        if (!overlay) return;

        const listEl = overlay.querySelector('#hwList');
        const sceneFilter = overlay.querySelector('#hwSceneFilter').value;
        const sourceFilter = overlay.querySelector('#hwSourceFilter').value;

        try {
            const result = await invoke('list_hotwords');
            let items = result.hotwords || [];

            // 前端过滤
            if (sceneFilter) items = items.filter(h => h.scene === sceneFilter);
            if (sourceFilter) items = items.filter(h => h.source === sourceFilter);

            if (items.length === 0) {
                listEl.innerHTML = '<div style="text-align:center;color:#b5b9cc;font-size:13px;padding:20px;">暂无热词</div>';
                return;
            }

            const sourceLabels = {
                'manual': '手动',
                'user_correction': '纠错',
                'imported': '导入',
                'auto': '自动'
            };
            const sceneLabels = {
                'general': '通用',
                'finance': '财务',
                'tech': '技术',
                'daily': '日常',
                'medical': '医疗',
                'legal': '法律'
            };

            listEl.innerHTML = items.map(h => `
                <div style="display:flex;align-items:center;gap:8px;padding:8px 10px;border-bottom:1px solid #f5f5f5;" data-id="${h.id}">
                    <div style="flex:1;min-width:0;">
                        <div style="display:flex;align-items:center;gap:6px;flex-wrap:wrap;">
                            <span style="color:#ef4444;font-size:13px;text-decoration:line-through;">${escapeHtmlLocal(h.wrong_text)}</span>
                            <span style="color:#9ca3af;font-size:12px;">→</span>
                            <span style="color:#15803d;font-size:13px;font-weight:500;">${escapeHtmlLocal(h.correct_text)}</span>
                        </div>
                        <div style="display:flex;gap:6px;margin-top:2px;">
                            <span style="font-size:11px;color:#9ca3af;background:#f3f4f6;padding:1px 6px;border-radius:8px;">${sourceLabels[h.source] || h.source}</span>
                            <span style="font-size:11px;color:#9ca3af;background:#f3f4f6;padding:1px 6px;border-radius:8px;">${sceneLabels[h.scene] || h.scene}</span>
                            ${h.frequency > 1 ? `<span style="font-size:11px;color:#6366f1;background:#eef2ff;padding:1px 6px;border-radius:8px;">命中${h.frequency}次</span>` : ''}
                        </div>
                    </div>
                    <button class="hw-del-btn" data-id="${h.id}" style="border:none;background:transparent;color:#ccc;font-size:16px;cursor:pointer;padding:4px 6px;" title="删除">×</button>
                </div>
            `).join('');

            // 绑定删除按钮
            listEl.querySelectorAll('.hw-del-btn').forEach(btn => {
                btn.addEventListener('click', async (e) => {
                    e.stopPropagation();
                    const id = btn.dataset.id;
                    try {
                        await invoke('delete_hotword', { hotwordId: id });
                        btn.closest('[data-id]').remove();
                        showToast('已删除');
                        loadStats();
                    } catch (err) {
                        showToast('删除失败: ' + err);
                    }
                });
            });

        } catch (err) {
            listEl.innerHTML = `<div style="text-align:center;color:#ef4444;font-size:13px;padding:20px;">加载失败: ${escapeHtmlLocal(String(err))}</div>`;
        }
    }

    // ==================== 添加热词 ====================
    async function handleAdd() {
        const overlay = document.getElementById('hotwordPanel');
        if (!overlay) return;

        const wrongEl = overlay.querySelector('#hwWrong');
        const correctEl = overlay.querySelector('#hwCorrect');
        const wrong = wrongEl.value.trim();
        const correct = correctEl.value.trim();

        if (!wrong || !correct) {
            showToast('请填写错误词和正确文本');
            return;
        }

        try {
            await invoke('add_hotword', {
                wrongText: wrong,
                correctText: correct,
                priority: 0,
                scene: 'general'
            });
            wrongEl.value = '';
            correctEl.value = '';
            wrongEl.focus();
            showToast('已添加');
            await loadList();
            loadStats();
        } catch (err) {
            showToast('添加失败: ' + err);
        }
    }

    // ==================== 批量导入 ====================
    async function handleImport() {
        const overlay = document.getElementById('hotwordPanel');
        if (!overlay) return;

        const text = overlay.querySelector('#hwImportText').value.trim();
        if (!text) {
            showToast('请输入热词内容');
            return;
        }

        const lines = text.split('\n').filter(l => l.trim());
        const hotwords = [];
        for (const line of lines) {
            const parts = line.split(/[,，]/);
            if (parts.length >= 2) {
                hotwords.push({
                    wrong_text: parts[0].trim(),
                    correct_text: parts[1].trim(),
                    scene: 'general'
                });
            }
        }

        if (hotwords.length === 0) {
            showToast('未解析到有效热词，请检查格式');
            return;
        }

        try {
            const result = await invoke('import_hotwords', { hotwords });
            showToast(`已导入 ${result.imported} 条`);
            overlay.querySelector('#hwImportText').value = '';
            overlay.querySelector('#hwImportBox').style.display = 'none';
            await loadList();
            loadStats();
        } catch (err) {
            showToast('导入失败: ' + err);
        }
    }

    // ==================== 导出 ====================
    async function handleExport() {
        const overlay = document.getElementById('hotwordPanel');
        if (!overlay) return;

        try {
            const result = await invoke('list_hotwords');
            const items = result.hotwords || [];
            if (items.length === 0) {
                showToast('暂无热词可导出');
                return;
            }
            const text = items.map(h => `${h.wrong_text},${h.correct_text}`).join('\n');
            // 复制到剪贴板
            await navigator.clipboard.writeText(text);
            showToast(`已复制 ${items.length} 条热词到剪贴板`);
        } catch (err) {
            showToast('导出失败: ' + err);
        }
    }

    // ==================== 加载纠错统计 ====================
    async function loadStats() {
        const overlay = document.getElementById('hotwordPanel');
        if (!overlay) return;

        const statsEl = overlay.querySelector('#hwStats');
        if (!statsEl) return;

        try {
            const stats = await invoke('get_correction_stats');

            if (!stats || stats.total_samples === 0) {
                statsEl.style.display = 'none';
                return;
            }

            statsEl.style.display = 'block';

            // 总数
            overlay.querySelector('#hwStatTotal').textContent = stats.total_samples;

            // 场景分布
            const sceneLabels = {
                'general': '通用', 'finance': '财务', 'tech': '技术',
                'legal': '法律', 'education': '教育', 'daily': '日常', 'medical': '医疗'
            };
            const scenesEl = overlay.querySelector('#hwStatScenes');
            if (stats.scenes && stats.scenes.length > 0) {
                scenesEl.innerHTML = stats.scenes.map(s =>
                    `<span style="font-size:11px;color:#6b7280;background:#fff;padding:2px 8px;border-radius:10px;border:1px solid #e5e7eb;">${sceneLabels[s.scene] || s.scene}: ${s.count}</span>`
                ).join('');
            } else {
                scenesEl.innerHTML = '';
            }

            // 最常错词 Top 5
            const topEl = overlay.querySelector('#hwStatTop');
            if (stats.top_errors && stats.top_errors.length > 0) {
                const top5 = stats.top_errors.slice(0, 5);
                topEl.innerHTML =
                    '<div style="font-size:11px;color:#9ca3af;margin-bottom:4px;">高频错词 Top 5</div>' +
                    top5.map(t =>
                        `<div style="display:flex;align-items:center;gap:4px;font-size:11px;color:#6b7280;margin-bottom:2px;">
                            <span style="color:#ef4444;text-decoration:line-through;">${escapeHtmlLocal(t.wrong)}</span>
                            <span>→</span>
                            <span style="color:#15803d;font-weight:500;">${escapeHtmlLocal(t.correct)}</span>
                            ${t.frequency > 1 ? `<span style="color:#6366f1;">(${t.frequency}次)</span>` : ''}
                        </div>`
                    ).join('');
            } else {
                topEl.innerHTML = '';
            }
        } catch (err) {
            // 统计加载失败不影响主功能，静默忽略
            console.warn('[hotwords] 加载统计失败:', err);
        }
    }

    // ==================== v2.6.0 纠错反哺建议加载 ====================
    async function loadFeedback() {
        const overlay = document.getElementById('hotwordPanel');
        if (!overlay) return;
        const boxEl = overlay.querySelector('#hwFeedback');
        const listEl = overlay.querySelector('#hwFeedbackList');
        if (!boxEl || !listEl) return;
        try {
            const r = await invoke('get_correction_hotword_suggestions');
            const sug = (r && r.suggestions) || [];
            if (!sug.length) { boxEl.style.display = 'none'; return; }
            boxEl.style.display = 'block';
            listEl._suggestions = sug;
            listEl.innerHTML = sug.slice(0, 20).map(s =>
                `<div style="display:flex;align-items:center;gap:4px;font-size:12px;color:#6b7280;margin-bottom:2px;">
                    <span style="color:#ef4444;">${escapeHtmlLocal(s.wrong)}</span>
                    <span>→</span>
                    <span style="color:#15803d;font-weight:500;">${escapeHtmlLocal(s.correct)}</span>
                    ${s.frequency > 1 ? `<span style="color:#0369a1;">（${s.frequency} 次）</span>` : ''}
                </div>`
            ).join('') + (sug.length > 20 ? `<div style="font-size:11px;color:#9ca3af;margin-top:2px;">…另有 ${sug.length - 20} 条</div>` : '');
        } catch (err) {
            boxEl.style.display = 'none';
        }
    }

    // ==================== v2.6.0 健康报告加载 ====================
    async function loadHealth() {
        const overlay = document.getElementById('hotwordPanel');
        if (!overlay) return;
        const boxEl = overlay.querySelector('#hwHealth');
        const bodyEl = overlay.querySelector('#hwHealthBody');
        if (!boxEl || !bodyEl) return;
        try {
            const h = await invoke('get_health_report');
            if (!h) { boxEl.style.display = 'none'; return; }
            boxEl.style.display = 'block';
            const t = h.today || {}, d = h.disk || {}, sv = h.services || {};
            const diskTxt = d.used_percent != null
                ? `磁盘 ${d.used_percent}%（水位线 ${d.watermark}%，audio_cache ${d.audio_cache_mb}MB）${d.used_percent >= d.watermark ? ' <span style="color:#ef4444;">超水位！</span>' : ''}`
                : '';
            bodyEl.innerHTML =
                `今日转写：${t.done || 0} 成功 · ${t.failed || 0} 失败 · ${t.silent || 0} 静音 · ${t.fallback || 0} 备用引擎<br>` +
                (h.backup && h.backup.last_auto_backup_date ? `最近自动备份：${h.backup.last_auto_backup_date}<br>` : `最近自动备份：<span style="color:#d97706;">从未（等今日首次运行）</span><br>`) +
                (diskTxt ? diskTxt + '<br>' : '') +
                `服务：ASR ${sv.asr_8083 === 'ok' ? '✓' : '<span style="color:#ef4444;">离线</span>'} · 本地大模型 ${sv.ollama === 'ok' ? '✓' : '<span style="color:#ef4444;">离线</span>'}`;
        } catch (err) {
            boxEl.style.display = 'none';
        }
    }

    // ==================== 工具函数 ====================
    function escapeHtmlLocal(s) {
        if (!s) return '';
        return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
    }

    function showToast(msg) {
        const existing = document.getElementById('hwToast');
        if (existing) existing.remove();
        const tip = document.createElement('div');
        tip.id = 'hwToast';
        tip.style.cssText = 'position:fixed;bottom:24px;left:50%;transform:translateX(-50%);background:rgba(0,0,0,.8);color:#fff;padding:10px 20px;border-radius:8px;font-size:14px;z-index:10000;transition:opacity .3s;';
        tip.textContent = msg;
        document.body.appendChild(tip);
        setTimeout(() => { tip.style.opacity = '0'; setTimeout(() => tip.remove(), 300); }, 2000);
    }

    // ==================== 初始化 ====================
    document.addEventListener('DOMContentLoaded', () => {
        const badge = $('hotwordBadge');
        if (badge) {
            badge.addEventListener('click', showPanel);
        }
    });

    // 暴露接口
    window.bijian = window.bijian || {};
    window.bijian.showHotwordPanel = showPanel;

})();
