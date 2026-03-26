// Commputer Desktop App — Block J (Items 176-200)
// Vanilla JS frontend communicating with the backend API and node RPC.

// API base points to the Rust HTTP server (same origin).
const API_BASE = '';
// Direct node RPC for any direct calls.
const RPC_BASE = 'http://127.0.0.1:9944';

let config = {
    contribution_percent: 100,
    rpc_port: 9944,
    auto_start: false,
    notifications: true,
    theme: 'system',
    log_level: 'info',
    data_dir: './commputer-testnet'
};

let walletSeedPhrase = []; // Held in memory only during creation
let wizardSeedPhrase = [];
let confirmWords = [];
let confirmIndex = 0;

// === Initialization ===

document.addEventListener('DOMContentLoaded', () => {
    loadConfig();
    applyTheme(config.theme);

    // Item 194: Check for first run / onboarding
    checkFirstRun();

    // Item 179: Contribution slider wired to config
    const slider = document.getElementById('contribution-slider');
    const sliderValue = document.getElementById('contribution-value');
    slider.value = config.contribution_percent;
    sliderValue.textContent = config.contribution_percent + '%';
    slider.addEventListener('input', () => {
        sliderValue.textContent = slider.value + '%';
        config.contribution_percent = parseInt(slider.value);
        saveContribution(parseInt(slider.value));
    });

    // Item 195: Keyboard shortcuts
    document.addEventListener('keydown', handleKeyboardShortcut);

    // Item 196: Restore window state (panels)
    restoreWindowState();

    // Start polling for live data (Item 176)
    setInterval(pollStatus, 3000);
    setInterval(pollMining, 5000);
    setInterval(pollTxHistory, 10000);
    setInterval(pollLogs, 5000);
    pollStatus();
    pollMining();
    pollTxHistory();

    // Item 192: Check for updates
    checkForUpdates();
});

// === Item 195: Keyboard Shortcuts ===

function handleKeyboardShortcut(e) {
    // Escape: close any open dialog
    if (e.key === 'Escape') {
        closeSettings();
        closeExportDialog();
        document.getElementById('wizard-overlay').classList.add('hidden');
        document.getElementById('onboarding-overlay').classList.add('hidden');
        return;
    }

    // Only handle Ctrl+ shortcuts when not in an input
    if (!e.ctrlKey && !e.metaKey) return;
    if (e.target.tagName === 'INPUT' || e.target.tagName === 'TEXTAREA') return;

    switch (e.key.toLowerCase()) {
        case 's':
            e.preventDefault();
            document.getElementById('send-to').focus();
            document.getElementById('send-panel').scrollIntoView({ behavior: 'smooth' });
            break;
        case 'w':
            e.preventDefault();
            document.getElementById('wallet-panel').scrollIntoView({ behavior: 'smooth' });
            break;
        case 'm':
            e.preventDefault();
            document.getElementById('mining-panel').scrollIntoView({ behavior: 'smooth' });
            break;
        case 'l':
            e.preventDefault();
            toggleLogViewer();
            break;
    }
}

// === Item 176: RPC Communication (via backend API) ===

async function apiGet(path) {
    try {
        const resp = await fetch(API_BASE + path);
        if (!resp.ok) return null;
        return await resp.json();
    } catch (e) {
        return null;
    }
}

async function apiPost(path, body) {
    try {
        const resp = await fetch(API_BASE + path, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(body)
        });
        if (!resp.ok) {
            const text = await resp.text();
            return { error: text };
        }
        return await resp.json();
    } catch (e) {
        return { error: e.message };
    }
}

// Direct RPC call (fallback for when backend is not running)
async function rpcGet(path) {
    try {
        const resp = await fetch(RPC_BASE + path);
        if (!resp.ok) return null;
        return await resp.json();
    } catch (e) {
        return null;
    }
}

// === Item 176: Status Polling (wired to real node RPC via backend) ===

async function pollStatus() {
    const data = await apiGet('/api/status');
    const dot = document.getElementById('status-dot');
    const text = document.getElementById('status-text');

    if (data && data.node_connected) {
        dot.className = 'dot green';
        text.textContent = 'Synced';
        document.getElementById('net-height').textContent = data.chain_height;
        document.getElementById('net-pending').textContent = data.pending_txs;
        document.getElementById('net-circulating').textContent = formatComme(data.circulating);
        document.getElementById('net-peers').textContent = data.peer_count;
        document.getElementById('mining-epoch').textContent = data.epoch;
        hideError();
    } else if (data) {
        dot.className = 'dot yellow';
        text.textContent = data.node_status || 'Error';
        showError('Node connection issue');
    } else {
        // Backend not running - try direct RPC
        const status = await rpcGet('/status');
        if (status) {
            dot.className = 'dot green';
            text.textContent = 'Synced';
            document.getElementById('net-height').textContent = status.height;
            document.getElementById('net-pending').textContent = status.pending_txs;
            document.getElementById('net-circulating').textContent = formatComme(status.circulating);
            document.getElementById('mining-epoch').textContent = status.epoch;
        } else {
            dot.className = 'dot red';
            text.textContent = 'Disconnected';
        }
    }

    // Item 184: Compliance status
    pollCompliance();
    // Item 186: Wallet info & tier progress
    pollWalletInfo();
}

// === Item 180: Mining Status with real proof scores ===

async function pollMining() {
    const data = await apiGet('/api/mining');
    if (!data || data.error) return;

    document.getElementById('mining-total').textContent = data.total_mined_formatted || '0 COMME';
    document.getElementById('mining-daily').textContent = data.daily_estimate_formatted || '0 COMME';

    if (data.proof_scores) {
        const maxScore = 100;
        updateProofBar('proof-cpu', 'proof-cpu-score', data.proof_scores.cpu, maxScore);
        updateProofBar('proof-gpu', 'proof-gpu-score', data.proof_scores.gpu, maxScore);
        updateProofBar('proof-storage', 'proof-storage-score', data.proof_scores.storage, maxScore);
        updateProofBar('proof-ram', 'proof-ram-score', data.proof_scores.ram, maxScore);
        updateProofBar('proof-bw', 'proof-bw-score', data.proof_scores.bandwidth, maxScore);
    }
}

function updateProofBar(barId, scoreId, score, maxScore) {
    const pct = Math.min(100, (score / maxScore) * 100);
    document.getElementById(barId).style.width = pct + '%';
    const scoreEl = document.getElementById(scoreId);
    if (scoreEl) scoreEl.textContent = score;
}

// === Item 184: Compliance from RPC ===

async function pollCompliance() {
    const data = await apiGet('/api/compliance');
    if (!data) return;

    const statusEl = document.getElementById('compliance-status');
    const explanationEl = document.getElementById('compliance-explanation');

    if (data.is_compliant) {
        statusEl.className = 'compliance-ok';
        statusEl.textContent = 'Compliant';
    } else {
        statusEl.className = 'compliance-nerfed';
        statusEl.textContent = data.status || 'Nerfed';
    }
    explanationEl.textContent = data.explanation || '';

    // Item 185: Grace period display
    if (data.grace_remaining_secs !== undefined) {
        const remaining = data.grace_remaining_secs || 0;
        const max = data.grace_max_secs || 1;
        const pct = (remaining / max) * 100;
        document.getElementById('grace-fill').style.height = pct + '%';
        document.getElementById('grace-text').textContent =
            remaining > 0 ? Math.floor(remaining / 60) + ' min remaining' : 'Full';
    }
}

// === Item 186: Wallet info with real balance & tier progress ===

async function pollWalletInfo() {
    const data = await apiGet('/api/wallet/info');
    if (!data || data.error) return;

    document.getElementById('wallet-address').textContent = data.address || '-';
    document.getElementById('wallet-balance').textContent = data.balance_formatted || '0 COMME';
    document.getElementById('wallet-tier').textContent = data.tier || 'None';

    if (data.tier_progress) {
        const tp = data.tier_progress;
        document.getElementById('tier-progress').style.width = tp.progress_percent + '%';
        const nextText = tp.next_tier
            ? formatComme(data.balance_raw || 0) + ' / ' + formatComme(tp.next_threshold || 0) + ' to ' + tp.next_tier
            : 'Maximum tier reached';
        document.getElementById('tier-next').textContent = nextText;
    }

    // Item 185: Grace period from wallet info
    if (data.grace_period) {
        const gp = data.grace_period;
        document.getElementById('grace-fill').style.height = gp.fill_percent + '%';
        const graceText = gp.is_draining
            ? Math.floor(gp.remaining_secs / 60) + ' min remaining'
            : 'Full';
        document.getElementById('grace-text').textContent = graceText;
    }
}

// === Item 182: Transaction history from RPC ===

async function pollTxHistory() {
    const data = await apiGet('/api/tx/history');
    if (!data || !data.transactions) return;

    const tbody = document.getElementById('tx-body');
    tbody.innerHTML = '';
    data.transactions.forEach(tx => {
        const row = document.createElement('tr');
        row.innerHTML = `
            <td>${escapeHtml(tx.tx_type)}</td>
            <td>${escapeHtml(tx.amount_formatted)}</td>
            <td>${formatTimestamp(tx.timestamp)}</td>
            <td><span class="tx-status tx-${tx.status}">${escapeHtml(tx.status)}</span></td>
        `;
        tbody.appendChild(row);
    });
}

// === Item 181: Send transaction wired to RPC ===

async function sendTransaction() {
    const to = document.getElementById('send-to').value.trim();
    const amount = parseFloat(document.getElementById('send-amount').value);
    const resultEl = document.getElementById('send-result');

    if (!to || to.length !== 64) {
        showSendResult('Please enter a valid 64-character hex address.', true);
        return;
    }
    if (!amount || amount <= 0) {
        showSendResult('Please enter a valid amount.', true);
        return;
    }

    if (!confirm('Send ' + amount + ' COMME to ' + to.substring(0, 8) + '...?')) return;

    const result = await apiPost('/api/tx/send', { to, amount });
    if (result && result.success) {
        showSendResult('Transaction submitted: ' + result.tx_hash, false);
        document.getElementById('send-to').value = '';
        document.getElementById('send-amount').value = '';
        pollTxHistory();
    } else {
        showSendResult('Failed: ' + (result ? result.error : 'Node unreachable'), true);
    }
}

function showSendResult(message, isError) {
    const el = document.getElementById('send-result');
    el.classList.remove('hidden');
    el.className = isError ? 'send-error' : 'send-success';
    el.textContent = message;
    setTimeout(() => el.classList.add('hidden'), 5000);
}

// === Item 177: Wallet Creation with real key generation ===

function checkFirstRun() {
    const address = localStorage.getItem('commputer_wallet_address');
    if (!address) {
        document.getElementById('wizard-overlay').classList.remove('hidden');
        // Item 194: Show onboarding after wallet creation
    } else {
        document.getElementById('wallet-address').textContent = address;
        // Show onboarding if not completed
        if (!localStorage.getItem('commputer_onboarding_done')) {
            setTimeout(() => showOnboarding(), 1000);
        }
    }
}

async function wizardCreate() {
    const result = await apiPost('/api/wallet/create', {});
    if (result && result.address) {
        wizardSeedPhrase = result.seed_phrase;
        walletSeedPhrase = result.seed_phrase;

        // Item 178: Display seed phrase with copy/print
        const display = document.getElementById('seed-phrase-display');
        display.innerHTML = '';
        result.seed_phrase.forEach((w, i) => {
            const span = document.createElement('span');
            span.textContent = (i + 1) + '. ' + w;
            display.appendChild(span);
        });

        localStorage.setItem('commputer_wallet_address', result.address);
        showWizardStep('wizard-step-2');
    } else {
        alert('Failed to create wallet. Is the backend running?');
    }
}

// Item 178: Copy seed phrase to clipboard
function copySeedPhrase() {
    const phrase = wizardSeedPhrase.join(' ');
    navigator.clipboard.writeText(phrase).then(() => {
        alert('Seed phrase copied to clipboard. Clear your clipboard after use!');
    }).catch(() => {
        // Fallback: select text
        const ta = document.createElement('textarea');
        ta.value = phrase;
        document.body.appendChild(ta);
        ta.select();
        document.execCommand('copy');
        document.body.removeChild(ta);
        alert('Seed phrase copied.');
    });
}

// Item 178: Print seed phrase
function printSeedPhrase() {
    const printWindow = window.open('', '_blank');
    if (!printWindow) {
        alert('Pop-up blocked. Please allow pop-ups to print.');
        return;
    }
    let html = '<html><head><title>Commputer Seed Phrase</title>';
    html += '<style>body{font-family:monospace;padding:40px;} .word{display:inline-block;width:180px;padding:4px 0;}</style>';
    html += '</head><body>';
    html += '<h2>Commputer Wallet Seed Phrase</h2>';
    html += '<p><strong>KEEP THIS SAFE. DO NOT SHARE.</strong></p>';
    html += '<div>';
    wizardSeedPhrase.forEach((w, i) => {
        html += '<span class="word">' + (i + 1) + '. ' + w + '</span>';
    });
    html += '</div>';
    html += '<p style="margin-top:20px;color:#999;">Generated: ' + new Date().toISOString() + '</p>';
    html += '</body></html>';
    printWindow.document.write(html);
    printWindow.document.close();
    printWindow.print();
}

function wizardRecover() {
    showWizardStep('wizard-step-recover');
}

function wizardConfirm() {
    confirmWords = [
        Math.floor(Math.random() * 24),
        Math.floor(Math.random() * 24),
        Math.floor(Math.random() * 24)
    ];
    while (confirmWords[1] === confirmWords[0]) confirmWords[1] = Math.floor(Math.random() * 24);
    while (confirmWords[2] === confirmWords[0] || confirmWords[2] === confirmWords[1])
        confirmWords[2] = Math.floor(Math.random() * 24);

    confirmIndex = 0;
    document.getElementById('confirm-word-num').textContent = confirmWords[0] + 1;
    document.getElementById('confirm-word-input').value = '';
    document.getElementById('confirm-error').classList.add('hidden');
    showWizardStep('wizard-step-3');
}

function wizardVerify() {
    const input = document.getElementById('confirm-word-input').value.trim().toLowerCase();
    const expected = wizardSeedPhrase[confirmWords[confirmIndex]];

    if (input !== expected) {
        document.getElementById('confirm-error').classList.remove('hidden');
        return;
    }

    document.getElementById('confirm-error').classList.add('hidden');
    confirmIndex++;

    if (confirmIndex < 3) {
        document.getElementById('confirm-word-num').textContent = confirmWords[confirmIndex] + 1;
        document.getElementById('confirm-word-input').value = '';
    } else {
        const address = localStorage.getItem('commputer_wallet_address');
        document.getElementById('wizard-address').textContent = address;
        showWizardStep('wizard-step-done');
    }
}

async function wizardDoRecover() {
    const phrase = document.getElementById('recover-phrase').value.trim();
    const words = phrase.split(/\s+/);
    if (words.length !== 24) {
        alert('Please enter exactly 24 words.');
        return;
    }

    const result = await apiPost('/api/wallet/recover', { seed_phrase: phrase });
    if (result && result.address) {
        localStorage.setItem('commputer_wallet_address', result.address);
        walletSeedPhrase = result.seed_phrase;
        document.getElementById('wizard-address').textContent = result.address;
        showWizardStep('wizard-step-done');
    } else {
        alert('Recovery failed: ' + (result ? result.error : 'Backend unreachable'));
    }
}

function wizardFinish() {
    document.getElementById('wizard-overlay').classList.add('hidden');
    document.getElementById('wallet-address').textContent =
        localStorage.getItem('commputer_wallet_address');
    // Show onboarding tutorial for new users
    if (!localStorage.getItem('commputer_onboarding_done')) {
        setTimeout(() => showOnboarding(), 500);
    }
}

function showWizardStep(stepId) {
    document.querySelectorAll('.wizard-step').forEach(el => el.classList.add('hidden'));
    document.getElementById(stepId).classList.remove('hidden');
}

// === Item 199: Export wallet ===

function showExportDialog() {
    document.getElementById('export-overlay').classList.remove('hidden');
    document.getElementById('export-result').classList.add('hidden');
    document.getElementById('export-actions').classList.remove('hidden');
}

function closeExportDialog() {
    document.getElementById('export-overlay').classList.add('hidden');
}

async function confirmExport() {
    if (!walletSeedPhrase || walletSeedPhrase.length === 0) {
        alert('No seed phrase available. You can only export right after creating or recovering your wallet in this session.');
        return;
    }

    const result = await apiPost('/api/wallet/export', {
        seed_phrase: walletSeedPhrase.join(' '),
        confirmed: true
    });

    if (result && result.seed_phrase) {
        const display = document.getElementById('export-seed-display');
        display.innerHTML = '';
        result.seed_phrase.forEach((w, i) => {
            const span = document.createElement('span');
            span.textContent = (i + 1) + '. ' + w;
            display.appendChild(span);
        });
        document.getElementById('export-result').classList.remove('hidden');
        document.getElementById('export-actions').classList.add('hidden');
    } else {
        alert('Export failed: ' + (result ? result.error : 'Unknown error'));
    }
}

function copyExportedSeed() {
    const spans = document.getElementById('export-seed-display').querySelectorAll('span');
    const words = Array.from(spans).map(s => s.textContent.split('. ')[1]);
    navigator.clipboard.writeText(words.join(' ')).then(() => {
        alert('Seed phrase copied. Clear your clipboard after use!');
    });
}

// === Item 183: Peer display ===

async function togglePeerMap() {
    const peerMap = document.getElementById('peer-map');
    if (!peerMap.classList.contains('hidden')) {
        peerMap.classList.add('hidden');
        return;
    }

    const data = await apiGet('/api/peers');
    if (!data) return;

    document.getElementById('peer-map-text').textContent = data.text_map || '';
    const tbody = document.getElementById('peer-table-body');
    tbody.innerHTML = '';
    if (data.peers) {
        data.peers.forEach(peer => {
            const row = document.createElement('tr');
            row.innerHTML = `
                <td title="${escapeHtml(peer.peer_id)}">${escapeHtml(peer.peer_id.substring(0, 16))}...</td>
                <td>${escapeHtml(peer.ip)}</td>
                <td><span class="peer-status peer-${peer.status}">${escapeHtml(peer.status)}</span></td>
            `;
            tbody.appendChild(row);
        });
    }
    document.getElementById('net-peers').textContent = data.peer_count || 0;
    peerMap.classList.remove('hidden');
}

// === Item 189: Dark/Light Theme ===

function applyTheme(theme) {
    document.body.className = 'theme-' + theme;
    localStorage.setItem('commputer_theme', theme);
}

function detectSystemTheme() {
    if (window.matchMedia && window.matchMedia('(prefers-color-scheme: light)').matches) {
        return 'light';
    }
    return 'dark';
}

// Listen for system theme changes
if (window.matchMedia) {
    window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', (e) => {
        if (config.theme === 'system') {
            // System theme class handles this via CSS, but we re-apply for consistency
            document.body.className = 'theme-system';
        }
    });
}

// === Item 193: Error display in status bar ===

function showError(message) {
    const bar = document.getElementById('error-bar');
    document.getElementById('error-message').textContent = message;
    bar.classList.remove('hidden');
}

function hideError() {
    document.getElementById('error-bar').classList.add('hidden');
}

function dismissError() {
    hideError();
}

// === Item 194: Onboarding tutorial ===

function showOnboarding() {
    document.querySelectorAll('.onboarding-step').forEach(el => el.classList.add('hidden'));
    document.getElementById('onboarding-step-1').classList.remove('hidden');
    document.getElementById('onboarding-overlay').classList.remove('hidden');
}

function onboardingNext(step) {
    document.querySelectorAll('.onboarding-step').forEach(el => el.classList.add('hidden'));
    document.getElementById('onboarding-step-' + step).classList.remove('hidden');
}

function onboardingPrev(step) {
    document.querySelectorAll('.onboarding-step').forEach(el => el.classList.add('hidden'));
    document.getElementById('onboarding-step-' + step).classList.remove('hidden');
}

function onboardingFinish() {
    document.getElementById('onboarding-overlay').classList.add('hidden');
    localStorage.setItem('commputer_onboarding_done', 'true');
}

// === Item 196: Window state persistence ===

function restoreWindowState() {
    try {
        const saved = localStorage.getItem('commputer_window_state');
        if (saved) {
            const state = JSON.parse(saved);
            // Restore which panels were open
            if (state.panels_open) {
                // All panels visible by default
            }
        }
    } catch (e) {}
}

function saveWindowState() {
    const panels = [];
    document.querySelectorAll('[data-panel]').forEach(el => {
        if (!el.classList.contains('hidden')) {
            panels.push(el.dataset.panel);
        }
    });
    const state = {
        panels_open: panels,
        width: window.innerWidth,
        height: window.innerHeight
    };
    localStorage.setItem('commputer_window_state', JSON.stringify(state));
    // Also save to backend
    apiPost('/api/config/window', {
        width: state.width,
        height: state.height,
        x: window.screenX || 0,
        y: window.screenY || 0,
        panels_open: panels
    });
}

window.addEventListener('beforeunload', saveWindowState);
window.addEventListener('resize', () => {
    clearTimeout(window._resizeTimer);
    window._resizeTimer = setTimeout(saveWindowState, 500);
});

// === Item 197: Log viewer ===

function toggleLogViewer() {
    const viewer = document.getElementById('log-viewer');
    viewer.classList.toggle('hidden');
    if (!viewer.classList.contains('hidden')) {
        pollLogs();
    }
}

async function pollLogs() {
    const viewer = document.getElementById('log-viewer');
    if (viewer.classList.contains('hidden')) return;

    const data = await apiGet('/api/logs');
    if (!data || !Array.isArray(data)) return;

    const container = document.getElementById('log-entries');
    container.innerHTML = '';
    data.forEach(log => {
        const div = document.createElement('div');
        div.className = 'log-entry log-' + log.level;
        div.textContent = '[' + log.level.toUpperCase() + '] ' + log.message;
        container.appendChild(div);
    });
    // Auto-scroll to bottom
    const logContainer = document.getElementById('log-container');
    logContainer.scrollTop = logContainer.scrollHeight;
}

function clearLogs() {
    document.getElementById('log-entries').innerHTML = '';
}

// === Item 192: Auto-update check ===

async function checkForUpdates() {
    const data = await apiGet('/api/update/check');
    if (data && data.update_available) {
        showError('Update available: v' + data.latest_version + ' - Visit GitHub to download');
    }
}

// === Item 179: Save contribution to backend ===

async function saveContribution(percent) {
    await apiPost('/api/config/contribution', { percent });
    saveConfig();
}

// === Settings ===

function openSettings() {
    document.getElementById('settings-overlay').classList.remove('hidden');
    document.getElementById('settings-contribution').value = config.contribution_percent;
    document.getElementById('settings-autostart').checked = config.auto_start;
    document.getElementById('settings-notifications').checked = config.notifications;
    document.getElementById('settings-theme').value = config.theme;
    document.getElementById('settings-loglevel').value = config.log_level;
    document.getElementById('settings-datadir').value = config.data_dir;
}

function closeSettings() {
    document.getElementById('settings-overlay').classList.add('hidden');
}

async function saveSettings() {
    config.contribution_percent = parseInt(document.getElementById('settings-contribution').value);
    config.auto_start = document.getElementById('settings-autostart').checked;
    config.notifications = document.getElementById('settings-notifications').checked;
    config.theme = document.getElementById('settings-theme').value;
    config.log_level = document.getElementById('settings-loglevel').value;
    config.data_dir = document.getElementById('settings-datadir').value;

    // Item 189: Apply theme immediately
    applyTheme(config.theme);
    saveConfig();

    // Save to backend (Item 190)
    await apiPost('/api/config', config);

    // Item 189: Theme change via API
    await apiPost('/api/config/theme', { theme: config.theme });

    closeSettings();

    document.getElementById('contribution-slider').value = config.contribution_percent;
    document.getElementById('contribution-value').textContent = config.contribution_percent + '%';
}

// === Config persistence (local + backend) ===

function loadConfig() {
    try {
        const saved = localStorage.getItem('commputer_config');
        if (saved) config = JSON.parse(saved);
    } catch (e) {}
    // Also try to load from backend
    apiGet('/api/config').then(data => {
        if (data && !data.error) {
            config = { ...config, ...data };
            applyTheme(config.theme);
        }
    });
}

function saveConfig() {
    try {
        localStorage.setItem('commputer_config', JSON.stringify(config));
    } catch (e) {}
}

// === Utilities ===

function formatComme(raw) {
    const UNITS = 100000000; // 10^8
    if (raw === undefined || raw === null) return '0 COMME';
    const whole = Math.floor(raw / UNITS);
    const frac = raw % UNITS;
    if (frac === 0) return whole.toLocaleString() + ' COMME';
    return whole.toLocaleString() + '.' + String(Math.floor(frac / (UNITS / 10000))).padStart(4, '0') + ' COMME';
}

function formatTimestamp(ts) {
    if (!ts) return '-';
    const d = new Date(ts * 1000);
    return d.toLocaleTimeString();
}

function escapeHtml(str) {
    if (!str) return '';
    const div = document.createElement('div');
    div.textContent = str;
    return div.innerHTML;
}
