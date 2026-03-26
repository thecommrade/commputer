// Commputer Desktop App — Items 22-40
// Vanilla JS frontend communicating with the node via RPC.

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

// === Initialization (Item 40: First-run experience) ===

document.addEventListener('DOMContentLoaded', () => {
    loadConfig();
    applyTheme(config.theme);

    // Item 40: Detect no wallet — show wizard
    checkFirstRun();

    // Item 23: Contribution slider
    const slider = document.getElementById('contribution-slider');
    const sliderValue = document.getElementById('contribution-value');
    slider.value = config.contribution_percent;
    sliderValue.textContent = config.contribution_percent + '%';
    slider.addEventListener('input', () => {
        sliderValue.textContent = slider.value + '%';
        config.contribution_percent = parseInt(slider.value);
        saveConfig();
    });

    // Start polling for updates
    setInterval(pollStatus, 2000);
    pollStatus();
});

// === RPC Communication ===

async function rpcGet(path) {
    try {
        const resp = await fetch(RPC_BASE + path);
        if (!resp.ok) return null;
        return await resp.json();
    } catch (e) {
        return null;
    }
}

async function rpcPost(path, body) {
    try {
        const resp = await fetch(RPC_BASE + path, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(body)
        });
        return await resp.json();
    } catch (e) {
        return null;
    }
}

// === Status Polling ===

async function pollStatus() {
    const status = await rpcGet('/status');
    const metrics = await rpcGet('/metrics');
    const health = await rpcGet('/health');

    // Item 32: Node status indicator
    const dot = document.getElementById('status-dot');
    const text = document.getElementById('status-text');
    if (status) {
        dot.className = 'dot green';
        text.textContent = 'Synced';
        updateNetworkStats(status);
    } else {
        dot.className = 'dot red';
        text.textContent = 'Disconnected';
    }

    // Update metrics if available
    if (metrics) {
        document.getElementById('net-peers').textContent = metrics.peers_connected || 0;
    }
}

function updateNetworkStats(status) {
    document.getElementById('net-height').textContent = status.height;
    document.getElementById('net-pending').textContent = status.pending_txs;
    document.getElementById('net-circulating').textContent = formatComme(status.circulating);
    document.getElementById('mining-epoch').textContent = status.epoch;
}

// === Item 101: Human-readable amounts ===

function formatComme(raw) {
    const UNITS = 10000000000; // 10^10
    const whole = Math.floor(raw / UNITS);
    const frac = raw % UNITS;
    if (frac === 0) return whole.toLocaleString() + ' COMME';
    return whole.toLocaleString() + '.' + String(Math.floor(frac / (UNITS / 10000))).padStart(4, '0') + ' COMME';
}

// === Item 28: Send transaction ===

async function sendTransaction() {
    const to = document.getElementById('send-to').value.trim();
    const amount = parseFloat(document.getElementById('send-amount').value);

    if (!to || to.length !== 64) {
        alert('Please enter a valid 64-character hex address.');
        return;
    }
    if (!amount || amount <= 0) {
        alert('Please enter a valid amount.');
        return;
    }

    // Confirmation dialog
    if (!confirm(`Send ${amount} COMME to ${to.substring(0, 8)}...?`)) return;

    const result = await rpcPost('/tx', { to, amount });
    if (result && result.success) {
        alert('Transaction submitted: ' + result.tx_hash);
        document.getElementById('send-to').value = '';
        document.getElementById('send-amount').value = '';
    } else {
        alert('Transaction failed: ' + (result ? result.error : 'Node unreachable'));
    }
}

// === Item 25: Wallet Creation Wizard ===

let wizardSeedPhrase = [];
let confirmWords = [];
let confirmIndex = 0;

function checkFirstRun() {
    // In a real Tauri app, this would check if a wallet file exists.
    // For now, check localStorage.
    if (!localStorage.getItem('commputer_wallet_address')) {
        document.getElementById('wizard-overlay').classList.remove('hidden');
    } else {
        document.getElementById('wallet-address').textContent =
            localStorage.getItem('commputer_wallet_address');
    }
}

function wizardCreate() {
    // Generate mock seed phrase (real implementation calls Rust backend)
    const words = ['abandon','ability','able','about','above','absent','absorb','abstract',
        'absurd','abuse','access','accident','account','accuse','achieve','acid',
        'acoustic','acquire','across','act','action','actor','actress','actual'];
    wizardSeedPhrase = words;

    const display = document.getElementById('seed-phrase-display');
    display.innerHTML = '';
    words.forEach((w, i) => {
        const span = document.createElement('span');
        span.textContent = (i + 1) + '. ' + w;
        display.appendChild(span);
    });

    showWizardStep('wizard-step-2');
}

function wizardRecover() {
    showWizardStep('wizard-step-recover');
}

function wizardConfirm() {
    // Pick 3 random words to confirm
    confirmWords = [
        Math.floor(Math.random() * 24),
        Math.floor(Math.random() * 24),
        Math.floor(Math.random() * 24)
    ];
    // Ensure unique
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
        // All confirmed — wallet created
        const address = '0'.repeat(64); // Mock address
        document.getElementById('wizard-address').textContent = address;
        localStorage.setItem('commputer_wallet_address', address);
        showWizardStep('wizard-step-done');
    }
}

function wizardDoRecover() {
    const phrase = document.getElementById('recover-phrase').value.trim();
    const words = phrase.split(/\s+/);
    if (words.length !== 24) {
        alert('Please enter exactly 24 words.');
        return;
    }
    // Mock recovery
    const address = '1'.repeat(64);
    document.getElementById('wizard-address').textContent = address;
    localStorage.setItem('commputer_wallet_address', address);
    showWizardStep('wizard-step-done');
}

function wizardFinish() {
    document.getElementById('wizard-overlay').classList.add('hidden');
    document.getElementById('wallet-address').textContent =
        localStorage.getItem('commputer_wallet_address');
}

function showWizardStep(stepId) {
    document.querySelectorAll('.wizard-step').forEach(el => el.classList.add('hidden'));
    document.getElementById(stepId).classList.remove('hidden');
}

// === Item 31: Theme ===

function applyTheme(theme) {
    document.body.className = 'theme-' + theme;
}

// === Item 30: Settings ===

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

function saveSettings() {
    config.contribution_percent = parseInt(document.getElementById('settings-contribution').value);
    config.auto_start = document.getElementById('settings-autostart').checked;
    config.notifications = document.getElementById('settings-notifications').checked;
    config.theme = document.getElementById('settings-theme').value;
    config.log_level = document.getElementById('settings-loglevel').value;
    config.data_dir = document.getElementById('settings-datadir').value;

    applyTheme(config.theme);
    saveConfig();
    closeSettings();

    // Update contribution slider
    document.getElementById('contribution-slider').value = config.contribution_percent;
    document.getElementById('contribution-value').textContent = config.contribution_percent + '%';
}

// === Config persistence ===

function loadConfig() {
    try {
        const saved = localStorage.getItem('commputer_config');
        if (saved) config = JSON.parse(saved);
    } catch (e) {}
}

function saveConfig() {
    try {
        localStorage.setItem('commputer_config', JSON.stringify(config));
    } catch (e) {}
}
