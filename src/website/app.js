// Commputer Network Dashboard
// Tries live RPC first, falls back to static JSON

const RPC_ENDPOINT = 'https://commputer.xyz/api';
const FALLBACK_JSON = 'stats.json';
const REFRESH_INTERVAL = 10000;

let lastUpdate = null;

// Format large numbers with commas
function formatNumber(n) {
    if (n === null || n === undefined || n === '—') return '—';
    return Number(n).toLocaleString();
}

// Format COMME amounts (raw units to human readable)
function formatComme(raw) {
    if (raw === null || raw === undefined) return '—';
    const comme = raw / 100000000;
    if (comme >= 1000000) return (comme / 1000000).toFixed(2) + 'M';
    if (comme >= 1000) return (comme / 1000).toFixed(2) + 'K';
    return comme.toFixed(4);
}

// Format time ago
function timeAgo(timestamp) {
    const seconds = Math.floor((Date.now() - timestamp) / 1000);
    if (seconds < 5) return 'just now';
    if (seconds < 60) return seconds + 's ago';
    if (seconds < 3600) return Math.floor(seconds / 60) + 'm ago';
    return Math.floor(seconds / 3600) + 'h ago';
}

// Update dashboard stats
function updateDashboard(data, source) {
    document.getElementById('stat-height').textContent = formatNumber(data.height);
    document.getElementById('stat-validators').textContent = formatNumber(data.validators || data.accounts || '—');
    document.getElementById('stat-epoch').textContent = formatNumber(data.epoch);
    document.getElementById('stat-circulating').textContent = formatComme(data.circulating);
    document.getElementById('stat-burned').textContent = formatComme(data.burned);
    document.getElementById('stat-remaining').textContent = formatComme(data.remaining);
    document.getElementById('stat-pending').textContent = formatNumber(data.pending_txs);

    const badge = document.getElementById('status-badge');
    const dot = badge.querySelector('.status-dot');
    const text = document.getElementById('status-text');

    if (source === 'live') {
        dot.className = 'status-dot online';
        text.textContent = 'Live';
    } else {
        dot.className = 'status-dot cached';
        text.textContent = 'Cached';
    }

    lastUpdate = Date.now();
    updateTimestamp();
}

function updateTimestamp() {
    if (lastUpdate) {
        document.getElementById('stat-note').textContent = 'Last updated: ' + timeAgo(lastUpdate);
    }
}

// Fetch from live RPC
async function fetchLive() {
    try {
        const response = await fetch(RPC_ENDPOINT + '/status', {
            signal: AbortSignal.timeout(5000)
        });
        if (!response.ok) throw new Error('RPC error');
        const data = await response.json();
        updateDashboard(data, 'live');
        return true;
    } catch (e) {
        return false;
    }
}

// Fetch from static JSON fallback
async function fetchFallback() {
    try {
        const response = await fetch(FALLBACK_JSON, {
            signal: AbortSignal.timeout(3000)
        });
        if (!response.ok) throw new Error('Fallback error');
        const data = await response.json();
        updateDashboard(data, 'cached');
        return true;
    } catch (e) {
        return false;
    }
}

// Show fetching state
function setFetching() {
    const dot = document.querySelector('.status-dot');
    const text = document.getElementById('status-text');
    if (dot) dot.className = 'status-dot fetching';
    if (text) text.textContent = 'Fetching...';
}

// Main refresh loop
async function refresh() {
    setFetching();
    const live = await fetchLive();
    if (!live) {
        const cached = await fetchFallback();
        if (!cached) {
            const dot = document.querySelector('.status-dot');
            const text = document.getElementById('status-text');
            dot.className = 'status-dot offline';
            text.textContent = 'Offline';
        }
    }
}

// OS detection for download
function detectOS() {
    const ua = navigator.userAgent.toLowerCase();
    const el = document.getElementById('detected-os');
    const btn = document.getElementById('download-btn');

    const base = 'https://github.com/thecommrade/commputer/releases/latest/download/';
    if (ua.includes('mac')) {
        el.textContent = 'macOS';
        btn.textContent = 'Download for macOS';
        btn.href = base + (ua.includes('arm') || ua.includes('aarch64') ? 'commputer-macos-aarch64' : 'commputer-macos-x86_64');
    } else if (ua.includes('linux')) {
        el.textContent = 'Linux';
        btn.textContent = 'Download for Linux';
        btn.href = base + 'commputer-linux-x86_64';
    } else if (ua.includes('win')) {
        el.textContent = 'Windows';
        btn.textContent = 'Download for Windows (coming soon)';
        btn.style.opacity = '0.5';
    } else {
        el.textContent = 'your platform';
    }
}

// Copy text to clipboard
function copyText(text) {
    navigator.clipboard.writeText(text).then(() => {
        // Brief visual feedback
        event.target.textContent = 'copied';
        setTimeout(() => { event.target.textContent = 'copy'; }, 1500);
    });
}

// Explorer search
async function explorerSearch() {
    const query = document.getElementById('explorer-input').value.trim();
    const result = document.getElementById('explorer-result');

    if (!query) return;

    result.innerHTML = '<p style="color: var(--text-dim)">Searching...</p>';

    try {
        // Try as block height
        if (/^\d+$/.test(query)) {
            const res = await fetch(RPC_ENDPOINT + '/block/' + query);
            if (res.ok) {
                const block = await res.json();
                result.innerHTML = renderBlock(block);
                return;
            }
        }

        // Try as address
        if (query.startsWith('comme:') || /^[a-f0-9]{16,}$/.test(query)) {
            const addr = query.replace('comme:', '');
            const res = await fetch(RPC_ENDPOINT + '/account/' + addr);
            if (res.ok) {
                const account = await res.json();
                result.innerHTML = renderAccount(account);
                return;
            }
        }

        result.innerHTML = '<p style="color: var(--text-dim)">No results found. Try a block height or address.</p>';
    } catch (e) {
        result.innerHTML = '<p style="color: var(--text-dim)">Explorer requires a live network connection.</p>';
    }
}

function renderBlock(block) {
    return `
        <div class="code-block" style="flex-direction: column; align-items: flex-start; gap: 4px; margin-bottom: 16px;">
            <div><span style="color: var(--text-dim)">Height:</span> ${block.height}</div>
            <div><span style="color: var(--text-dim)">Hash:</span> ${block.hash}</div>
            <div><span style="color: var(--text-dim)">Time:</span> ${new Date(block.timestamp * 1000).toISOString()}</div>
            <div><span style="color: var(--text-dim)">Transactions:</span> ${block.tx_count || 0}</div>
        </div>
    `;
}

function renderAccount(account) {
    return `
        <div class="code-block" style="flex-direction: column; align-items: flex-start; gap: 4px; margin-bottom: 16px;">
            <div><span style="color: var(--text-dim)">Address:</span> ${account.address}</div>
            <div><span style="color: var(--text-dim)">Balance:</span> ${formatComme(account.balance)} COMME</div>
            <div><span style="color: var(--text-dim)">Tier:</span> ${account.tier || 'None'}</div>
            <div><span style="color: var(--text-dim)">Transactions:</span> ${account.tx_count || 0}</div>
        </div>
    `;
}

// Initialize
document.addEventListener('DOMContentLoaded', () => {
    detectOS();
    refresh();
    setInterval(refresh, REFRESH_INTERVAL);
    setInterval(updateTimestamp, 1000);
});
