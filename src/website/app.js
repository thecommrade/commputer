// Commputer Network Dashboard
// Fetches live data from the RPC proxy at api.commputer.xyz

// Item 36: Point to actual seed node RPC (via Cloudflare Worker proxy)
const RPC_ENDPOINT = 'https://api.commputer.xyz';
const FALLBACK_JSON = 'stats.json';
const REFRESH_INTERVAL = 10000; // Item 38: auto-refresh every 10 seconds

let lastUpdate = null;
let isLive = false;

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
    if (comme >= 1) return comme.toFixed(4);
    return comme.toFixed(8);
}

// Format time ago
function timeAgo(timestamp) {
    const seconds = Math.floor((Date.now() - timestamp) / 1000);
    if (seconds < 5) return 'just now';
    if (seconds < 60) return seconds + 's ago';
    if (seconds < 3600) return Math.floor(seconds / 60) + 'm ago';
    return Math.floor(seconds / 3600) + 'h ago';
}

// Item 48: Show loading skeleton
function showLoading() {
    const ids = ['stat-height', 'stat-validators', 'stat-epoch', 'stat-circulating', 'stat-burned', 'stat-remaining', 'stat-pending'];
    ids.forEach(id => {
        const el = document.getElementById(id);
        if (el && el.textContent === '—') {
            el.classList.add('loading');
        }
    });
}

function hideLoading() {
    document.querySelectorAll('.loading').forEach(el => el.classList.remove('loading'));
}

// Update dashboard stats
function updateDashboard(data, source) {
    hideLoading();
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
        isLive = true;
    } else {
        dot.className = 'status-dot cached';
        text.textContent = 'Cached';
        isLive = false;
    }

    lastUpdate = Date.now();
    updateTimestamp();
}

function updateTimestamp() {
    if (lastUpdate) {
        document.getElementById('stat-note').textContent = 'Last updated: ' + timeAgo(lastUpdate);
    }
}

// Item 49: Show error state
function showError(msg) {
    hideLoading();
    const dot = document.querySelector('.status-dot');
    const text = document.getElementById('status-text');
    if (dot) dot.className = 'status-dot offline';
    if (text) text.textContent = msg || 'Network offline';
}

// Fetch from live RPC
async function fetchLive() {
    try {
        const response = await fetch(RPC_ENDPOINT + '/status', {
            signal: AbortSignal.timeout(5000)
        });
        if (!response.ok) throw new Error('RPC error');
        const data = await response.json();

        // Item 44: Also fetch validator count
        try {
            const vRes = await fetch(RPC_ENDPOINT + '/validators', { signal: AbortSignal.timeout(3000) });
            if (vRes.ok) {
                const vData = await vRes.json();
                data.validators = vData.count || 0;
            }
        } catch (e) { /* non-critical */ }

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

// Item 39: Fetch and display recent blocks
async function fetchRecentBlocks() {
    const container = document.getElementById('recent-blocks');
    if (!container) return;

    try {
        const res = await fetch(RPC_ENDPOINT + '/blocks?limit=10', { signal: AbortSignal.timeout(5000) });
        if (!res.ok) throw new Error('blocks error');
        const data = await res.json();

        if (!data.blocks || data.blocks.length === 0) {
            container.innerHTML = '<p style="color: var(--text-dim); text-align: center;">No blocks yet — waiting for network...</p>';
            return;
        }

        let html = '<table class="block-table"><thead><tr><th>Height</th><th>Txs</th><th>Time</th><th>Producer</th></tr></thead><tbody>';
        for (const block of data.blocks) {
            const time = block.timestamp ? new Date(block.timestamp * 1000).toLocaleTimeString() : '—';
            const producer = block.producer ? truncAddr(JSON.stringify(block.producer)) : 'genesis';
            html += `<tr>
                <td><a href="#" onclick="searchBlock(${block.height}); return false;">${block.height}</a></td>
                <td>${block.tx_count || 0}</td>
                <td>${time}</td>
                <td class="mono">${producer}</td>
            </tr>`;
        }
        html += '</tbody></table>';
        container.innerHTML = html;
    } catch (e) {
        container.innerHTML = '<p style="color: var(--text-dim); text-align: center;">Block data unavailable</p>';
    }
}

function truncAddr(s) {
    if (s.length > 16) return s.substring(0, 12) + '...';
    return s;
}

// Item 47: Update footer chain height
function updateFooterHeight(height) {
    const el = document.getElementById('footer-height');
    if (el && height) {
        el.textContent = 'Chain height: ' + formatNumber(height);
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
            showError('Network offline');
        }
    }

    // Item 39: Fetch recent blocks
    await fetchRecentBlocks();

    // Item 47: Update footer height
    const heightEl = document.getElementById('stat-height');
    if (heightEl) {
        updateFooterHeight(heightEl.textContent);
    }
}

// OS detection for download
function detectOS() {
    const ua = navigator.userAgent.toLowerCase();
    const el = document.getElementById('detected-os');
    const btn = document.getElementById('download-btn');

    const base = 'https://github.com/thecommrade/commputer/releases/latest/download/';
    if (ua.includes('mac')) {
        if (el) el.textContent = 'macOS';
        if (btn) {
            btn.textContent = 'Download for macOS';
            btn.href = base + (ua.includes('arm') || ua.includes('aarch64') ? 'commputer-macos-aarch64' : 'commputer-macos-x86_64');
        }
    } else if (ua.includes('linux')) {
        if (el) el.textContent = 'Linux';
        if (btn) {
            btn.textContent = 'Download for Linux';
            btn.href = base + 'commputer-linux-x86_64';
        }
    } else if (ua.includes('win')) {
        if (el) el.textContent = 'Windows';
        if (btn) {
            btn.textContent = 'Download for Windows (coming soon)';
            btn.style.opacity = '0.5';
        }
    } else {
        if (el) el.textContent = 'your platform';
    }
}

// Copy text to clipboard
function copyText(text) {
    navigator.clipboard.writeText(text).then(() => {
        event.target.textContent = 'copied';
        setTimeout(() => { event.target.textContent = 'copy'; }, 1500);
    });
}

// Explorer search
async function explorerSearch() {
    const input = document.getElementById('explorer-input');
    const query = input ? input.value.trim() : '';
    const result = document.getElementById('explorer-result');

    if (!query || !result) return;

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

        // Try as address (64 hex chars)
        if (/^[a-f0-9]{64}$/i.test(query) || query.startsWith('comme:')) {
            const addr = query.replace('comme:', '');
            const res = await fetch(RPC_ENDPOINT + '/account/' + addr);
            if (res.ok) {
                const account = await res.json();
                result.innerHTML = renderAccount(account);
                return;
            }
        }

        result.innerHTML = '<p style="color: var(--text-dim)">No results found. Try a block height or 64-char hex address.</p>';
    } catch (e) {
        result.innerHTML = '<p style="color: var(--text-dim)">Explorer requires a live network connection.</p>';
    }
}

function searchBlock(height) {
    const input = document.getElementById('explorer-input');
    if (input) {
        input.value = height;
        explorerSearch();
    }
}

function renderBlock(block) {
    const header = block.header || block;
    const height = header.height || block.height || '?';
    const timestamp = header.timestamp ? new Date(header.timestamp * 1000).toISOString() : '?';
    const txCount = (block.transactions || []).length || block.tx_count || 0;

    return `
        <div class="code-block" style="flex-direction: column; align-items: flex-start; gap: 4px; margin-bottom: 16px;">
            <div><span style="color: var(--text-dim)">Height:</span> ${height}</div>
            <div><span style="color: var(--text-dim)">Time:</span> ${timestamp}</div>
            <div><span style="color: var(--text-dim)">Epoch:</span> ${header.epoch || '?'}</div>
            <div><span style="color: var(--text-dim)">Transactions:</span> ${txCount}</div>
            <div><span style="color: var(--text-dim)">Chain ID:</span> ${header.chain_id || '?'}</div>
        </div>
    `;
}

function renderAccount(account) {
    return `
        <div class="code-block" style="flex-direction: column; align-items: flex-start; gap: 4px; margin-bottom: 16px;">
            <div><span style="color: var(--text-dim)">Address:</span> ${account.address}</div>
            <div><span style="color: var(--text-dim)">Balance:</span> ${account.balance_comme || formatComme(account.balance)} COMME</div>
            <div><span style="color: var(--text-dim)">Tier:</span> ${account.tier || 'None'}</div>
            <div><span style="color: var(--text-dim)">Validator:</span> ${account.is_validator ? 'Yes' : 'No'}</div>
            <div><span style="color: var(--text-dim)">Nonce:</span> ${account.nonce || 0}</div>
            <div><span style="color: var(--text-dim)">Total Mined:</span> ${account.total_mined_comme || formatComme(account.total_mined)} COMME</div>
        </div>
    `;
}

// Initialize
document.addEventListener('DOMContentLoaded', () => {
    showLoading();
    detectOS();
    refresh();
    setInterval(refresh, REFRESH_INTERVAL);
    setInterval(updateTimestamp, 1000);
});
