// Cloudflare Worker — reverse proxy for api.commputer.xyz
// Proxies requests to the seed node RPC, adds CORS headers, hides seed IP.
//
// Deploy: wrangler publish
// Route: api.commputer.xyz/*
//
// Environment variables (set in Cloudflare dashboard or wrangler.toml):
//   SEED_RPC_URL — e.g. "http://198.51.100.254:9944"

const ALLOWED_ORIGINS = [
    'https://commputer.xyz',
    'https://www.commputer.xyz',
    'http://localhost:3000',
    'http://localhost:8000',
];

// Only proxy these safe, read-only endpoints
const ALLOWED_PATHS = [
    '/status',
    '/health',
    '/peers',
    '/validators',
    '/network',
    '/network/info',
    '/network/quality',
    '/blocks',
    '/supply',
    '/leaderboard',
    '/stats',
    '/metrics',
    '/fee-estimate',
    '/compliance',
    '/proofs/status',
    '/proofs/leaderboard',
    '/traffic',
];

// Paths that take a parameter
const ALLOWED_PARAM_PATHS = [
    '/block/',
    '/balance/',
    '/account/',
    '/nonce/',
    '/receipt/',
    '/rewards/',
    '/validator/',
    '/proofs/history/',
];

export default {
    async fetch(request, env) {
        const url = new URL(request.url);
        const origin = request.headers.get('Origin') || '';

        // CORS preflight
        if (request.method === 'OPTIONS') {
            return new Response(null, {
                headers: corsHeaders(origin),
            });
        }

        // Check if path is allowed
        const path = url.pathname;
        const isAllowed = ALLOWED_PATHS.includes(path) ||
            ALLOWED_PARAM_PATHS.some(p => path.startsWith(p));

        if (!isAllowed) {
            return new Response(JSON.stringify({ error: 'endpoint not proxied' }), {
                status: 403,
                headers: { 'Content-Type': 'application/json', ...corsHeaders(origin) },
            });
        }

        // Proxy to seed node
        const seedUrl = (env.SEED_RPC_URL || 'http://127.0.0.1:9944') + path + url.search;

        try {
            const resp = await fetch(seedUrl, {
                method: request.method,
                headers: { 'Content-Type': 'application/json' },
            });

            const body = await resp.text();

            return new Response(body, {
                status: resp.status,
                headers: {
                    'Content-Type': 'application/json',
                    'Cache-Control': 'public, max-age=2',
                    ...corsHeaders(origin),
                },
            });
        } catch (e) {
            return new Response(JSON.stringify({ error: 'seed node unreachable' }), {
                status: 502,
                headers: { 'Content-Type': 'application/json', ...corsHeaders(origin) },
            });
        }
    },
};

function corsHeaders(origin) {
    const allowOrigin = ALLOWED_ORIGINS.includes(origin) ? origin : '*';
    return {
        'Access-Control-Allow-Origin': allowOrigin,
        'Access-Control-Allow-Methods': 'GET, OPTIONS',
        'Access-Control-Allow-Headers': 'Content-Type',
        'Access-Control-Max-Age': '86400',
    };
}
