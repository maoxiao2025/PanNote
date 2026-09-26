/**
 * PanNote - 许可证管理模块
 * 激活码输入、状态显示、退出激活
 */
(function() {
    'use strict';
    
    const { api, $, eventBus } = window.bijian;
    
    let licenseStatus = null;
    
    // ==================== 刷新徽章 ====================
    async function refreshBadge() {
        const badge = $('licenseBadge');
        if (!badge) return;
        try {
            licenseStatus = await api('license/status');
            if (licenseStatus.is_active) {
                badge.textContent = 'Pro';
                badge.classList.add('pro');
                badge.classList.remove('free');
            } else {
                badge.textContent = '免费版';
                badge.classList.add('free');
                badge.classList.remove('pro');
            }
        } catch {
            badge.textContent = '免费版';
            badge.classList.add('free');
        }
    }
    
    // ==================== 许可证面板 ====================
    function showPanel() {
        const existing = document.getElementById('licensePanel');
        if (existing) existing.remove();
        
        const overlay = document.createElement('div');
        overlay.id = 'licensePanel';
        overlay.style.cssText = 'position:fixed;inset:0;background:rgba(0,0,0,.45);z-index:9999;display:flex;align-items:center;justify-content:center;';
        
        const status = licenseStatus || {};
        const isActive = status.is_active;
        const tierLabel = status.tier === 'pro' ? 'Pro 版' : '免费版';
        const expiryText = status.expires_human ? `到期：${status.expires_human}` : '永久有效';
        
        // 功能列表
        const featuresHtml = (status.features || []).map(f => `
            <div class="license-feature-row">
                <span class="license-feature-dot ${f.enabled ? 'on' : 'off'}"></span>
                <span class="license-feature-name">${f.name}</span>
                <span class="license-feature-tier ${f.tier}">${f.tier === 'pro' ? 'Pro' : '免费'}</span>
            </div>
        `).join('');
        
        overlay.innerHTML = `
            <div style="background:#fff;border-radius:16px;padding:28px 32px;max-width:480px;width:90%;box-shadow:0 12px 40px rgba(0,0,0,.2);max-height:85vh;overflow-y:auto;">
                <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:20px;">
                    <h2 style="font-size:18px;font-weight:600;margin:0;">许可证管理</h2>
                    <button id="licClose" style="border:none;background:transparent;font-size:20px;cursor:pointer;color:#999;padding:0 4px;">✕</button>
                </div>
                
                <div style="background:${isActive ? '#f0fdf4' : '#f9fafb'};border-radius:12px;padding:16px;margin-bottom:20px;">
                    <div style="display:flex;align-items:center;gap:8px;margin-bottom:4px;">
                        <span style="font-size:15px;font-weight:600;color:${isActive ? '#15803d' : '#6b7280'};">${tierLabel}</span>
                        ${isActive ? '<span style="font-size:11px;background:#15803d;color:#fff;padding:2px 8px;border-radius:10px;">已激活</span>' : ''}
                    </div>
                    ${isActive ? `<div style="font-size:12px;color:#6b7280;">${expiryText}</div>` : '<div style="font-size:12px;color:#9ca3af;">升级 Pro 解锁全部功能</div>'}
                </div>
                
                ${isActive ? '' : `
                <div style="margin-bottom:20px;">
                    <label style="font-size:13px;color:#374151;display:block;margin-bottom:6px;">输入激活码</label>
                    <textarea id="licKeyInput" placeholder="粘贴激活码…" style="width:100%;height:70px;border:1px solid #d1d5db;border-radius:8px;padding:10px 12px;font-size:12px;font-family:monospace;resize:none;box-sizing:border-box;" autocomplete="off"></textarea>
                    <div id="licError" style="color:#ef4444;font-size:12px;margin-top:4px;display:none;"></div>
                    <button id="licActivate" style="margin-top:10px;width:100%;padding:10px;border:none;border-radius:8px;background:#6366f1;color:#fff;font-size:14px;cursor:pointer;">激活</button>
                </div>
                `}
                
                ${isActive ? `
                <div style="margin-bottom:20px;">
                    <button id="licDeactivate" style="width:100%;padding:10px;border:1px solid #e5e7eb;border-radius:8px;background:transparent;color:#6b7280;font-size:13px;cursor:pointer;">退出 Pro 模式</button>
                </div>
                ` : ''}
                
                <div>
                    <div style="font-size:13px;font-weight:600;color:#374151;margin-bottom:10px;">功能列表</div>
                    <div style="display:flex;flex-direction:column;gap:6px;">
                        ${featuresHtml}
                    </div>
                </div>
            </div>
        `;
        
        document.body.appendChild(overlay);
        
        // 绑定事件
        overlay.querySelector('#licClose').addEventListener('click', () => overlay.remove());
        overlay.addEventListener('click', (e) => { if (e.target === overlay) overlay.remove(); });
        
        const activateBtn = overlay.querySelector('#licActivate');
        if (activateBtn) {
            activateBtn.addEventListener('click', async () => {
                const key = overlay.querySelector('#licKeyInput').value.trim();
                if (!key) return;
                activateBtn.textContent = '激活中…';
                activateBtn.disabled = true;
                try {
                    const result = await api('license/activate', { key });
                    if (result.success) {
                        overlay.remove();
                        refreshBadge();
                        // 轻提示
                        const tip = document.createElement('div');
                        tip.style.cssText = 'position:fixed;bottom:24px;left:50%;transform:translateX(-50%);background:#15803d;color:#fff;padding:10px 20px;border-radius:8px;font-size:14px;z-index:10000;';
                        tip.textContent = '激活成功！已升级为 Pro 版';
                        document.body.appendChild(tip);
                        setTimeout(() => tip.remove(), 2500);
                    } else {
                        const errEl = overlay.querySelector('#licError');
                        errEl.textContent = result.message || '激活失败';
                        errEl.style.display = 'block';
                        activateBtn.textContent = '激活';
                        activateBtn.disabled = false;
                    }
                } catch (e) {
                    const errEl = overlay.querySelector('#licError');
                    errEl.textContent = '激活失败: ' + e.message;
                    errEl.style.display = 'block';
                    activateBtn.textContent = '激活';
                    activateBtn.disabled = false;
                }
            });
        }
        
        const deactivateBtn = overlay.querySelector('#licDeactivate');
        if (deactivateBtn) {
            deactivateBtn.addEventListener('click', async () => {
                deactivateBtn.textContent = '处理中…';
                deactivateBtn.disabled = true;
                try {
                    await api('license/deactivate');
                    overlay.remove();
                    refreshBadge();
                    const tip = document.createElement('div');
                    tip.style.cssText = 'position:fixed;bottom:24px;left:50%;transform:translateX(-50%);background:rgba(0,0,0,.8);color:#fff;padding:10px 20px;border-radius:8px;font-size:14px;z-index:10000;';
                    tip.textContent = '已退出 Pro 模式';
                    document.body.appendChild(tip);
                    setTimeout(() => tip.remove(), 2000);
                } catch (e) {
                    deactivateBtn.textContent = '退出 Pro 模式';
                    deactivateBtn.disabled = false;
                }
            });
        }
    }
    
    // ==================== 初始化 ====================
    document.addEventListener('DOMContentLoaded', () => {
        refreshBadge();
    });
    
    // 暴露接口
    window.bijian = window.bijian || {};
    window.bijian.refreshLicenseBadge = refreshBadge;
    window.bijian.showLicensePanel = showPanel;
})();
