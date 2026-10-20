// The admin dashboard: polls /admin/api/overview and renders it. Everything
// is built with DOM calls, so values from the database are never parsed as
// HTML.
'use strict';

const POLL_MS = 5000;
// Transactions per second kept for the sparkline: 5 minutes of polls.
const HISTORY = 60;
// NETWORK_PERSISTENT_* (docs/gamestats2.md, "Categories").
const CATEGORIES = [
  'Season',
  'Striker of the day',
  'Friends (EU)',
  'Alternate season (EU)',
  'Alternate striker of the day (EU)',
];
const SEVERITY = ['good', 'warning', 'serious', 'critical'];

const integer = new Intl.NumberFormat();
const compact = new Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 1 });
// Exact below 10,000, then 12.9K.
const count = (value) => (value < 10000 ? integer : compact).format(value);
const percent = new Intl.NumberFormat(undefined, { style: 'percent', maximumFractionDigits: 1 });
const clock = new Intl.DateTimeFormat(undefined, { timeStyle: 'medium' });
const dateTime = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' });

const ICONS = {
  good: 'M20 6 9 17l-5-5',
  warning: 'M12 9v4m0 4h.01M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z',
  serious: 'M12 9v4m0 4h.01M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z',
  critical: 'M18 6 6 18M6 6l12 12',
  pending: 'M12 6v6l4 2',
};

let timer = null;
let previous = null; // The last counters, for transactions per second.
let samples = []; // { at, rate } for the sparkline.
let hover = null; // The pointer's position across the sparkline, 0 to 1.

// --- DOM helpers ---------------------------------------------------------

function h(tag, props, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props || {})) {
    if (value == null || value === false) continue;
    if (key === 'class') node.className = value;
    else if (key.startsWith('on')) node.addEventListener(key.slice(2), value);
    else node.setAttribute(key, value === true ? '' : value);
  }
  node.append(...children.flat().filter((child) => child != null && child !== false));
  return node;
}

function svg(tag, attributes, ...children) {
  const node = document.createElementNS('http://www.w3.org/2000/svg', tag);
  for (const [key, value] of Object.entries(attributes || {})) node.setAttribute(key, value);
  node.append(...children);
  return node;
}

function icon(level) {
  return svg(
    'svg',
    { viewBox: '0 0 24 24', fill: 'none', stroke: 'currentColor', 'stroke-width': '2.25', 'stroke-linecap': 'round', 'stroke-linejoin': 'round' },
    svg('path', { d: ICONS[level] || ICONS.pending }),
  );
}

// --- Formatting ----------------------------------------------------------

function bytes(value) {
  const units = ['B', 'kB', 'MB', 'GB', 'TB'];
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${unit > 0 && value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}

function duration(seconds) {
  const s = Math.max(0, Math.floor(seconds));
  const days = Math.floor(s / 86400);
  const hours = Math.floor((s % 86400) / 3600);
  const minutes = Math.floor((s % 3600) / 60);
  if (days) return `${days}d ${hours}h`;
  if (hours) return `${hours}h ${minutes}m`;
  if (minutes) return `${minutes}m ${s % 60}s`;
  return `${s}s`;
}

function since(iso, now) {
  return (now - Date.parse(iso)) / 1000;
}

/** A <time> that says how long ago, with the date on hover. */
function ago(iso, now) {
  if (!iso) return h('span', { class: 'sub' }, 'never');
  const seconds = since(iso, now);
  const text = seconds < 45 ? 'just now' : `${duration(seconds).split(' ')[0]} ago`;
  return h('time', { datetime: iso, title: dateTime.format(new Date(iso)) }, text);
}

function shortVersion(version) {
  return version.split(' ')[0];
}

// --- Rendering -----------------------------------------------------------

function table(columns, rows, empty) {
  if (!rows.length) return h('p', { class: 'empty' }, empty);
  return h(
    'table',
    null,
    h('thead', null, h('tr', null, columns.map((column) => h('th', { class: column.num ? 'num' : null, scope: 'col' }, column.label)))),
    h(
      'tbody',
      null,
      rows.map((row) =>
        h('tr', null, columns.map((column) => h('td', { class: column.class || (column.num ? 'num' : null) }, column.cell(row)))),
      ),
    ),
  );
}

function health(data, now) {
  const db = data.database;
  if (!db) {
    return { level: 'critical', title: 'Database unreachable', detail: data.database_error || 'No answer.' };
  }
  const issues = [];
  const usage = db.open_connections / db.max_connections;
  if (usage >= 0.9) issues.push(['serious', `Connections at ${percent.format(usage)} of max_connections`]);
  else if (usage >= 0.75) issues.push(['warning', `Connections at ${percent.format(usage)} of max_connections`]);
  if (db.migrations.some((migration) => !migration.success)) issues.push(['critical', 'A migration failed']);
  if (!issues.length) {
    return {
      level: 'good',
      title: 'Database healthy',
      detail: `PostgreSQL ${shortVersion(db.version)} · ${db.name} · up ${duration(since(db.started_at, now))}`,
    };
  }
  issues.sort((a, b) => SEVERITY.indexOf(b[0]) - SEVERITY.indexOf(a[0]));
  return {
    level: issues[0][0],
    title: issues[0][1],
    detail: issues.slice(1).map((issue) => issue[1]).join(' · ') || `PostgreSQL ${shortVersion(db.version)} · ${db.name}`,
  };
}

function renderStatus(data, now) {
  const { level, title, detail } = health(data, now);
  const status = document.getElementById('status');
  status.dataset.level = level;
  status.replaceChildren(
    h('span', { class: 'status-icon', 'aria-hidden': 'true' }, icon(level)),
    h('div', { class: 'status-text' }, h('strong', null, title), h('span', null, detail)),
  );
}

function tile(label, value, sub, ...extra) {
  return h('article', { class: 'tile' }, h('span', { class: 'tile-label' }, label), h('span', { class: 'tile-value' }, value), ...extra, h('span', { class: 'tile-sub' }, sub));
}

function sparkline() {
  const rates = samples.map((sample) => sample.rate);
  const top = Math.max(1, ...rates);
  const width = HISTORY - 1;
  const height = 36;
  // Newest at the right edge; y leaves room for the 2px stroke.
  const x = (index) => width - (rates.length - 1 - index);
  const y = (rate) => height - 2 - (rate / top) * (height - 4);
  const points = rates.map((rate, index) => `${x(index)},${y(rate).toFixed(2)}`).join(' ');

  const plot = svg('svg', { viewBox: `0 0 ${width} ${height}`, preserveAspectRatio: 'none', 'aria-hidden': 'true' });
  plot.append(svg('line', { class: 'spark-base', x1: 0, x2: width, y1: height - 0.5, y2: height - 0.5, 'vector-effect': 'non-scaling-stroke' }));
  if (rates.length > 1) {
    plot.append(
      svg('polygon', { class: 'spark-area', points: `${x(0)},${height} ${points} ${width},${height}` }),
      svg('polyline', { class: 'spark-line', points, 'vector-effect': 'non-scaling-stroke' }),
    );
  }

  const box = h('div', { class: 'spark', role: 'img', 'aria-label': `Transactions per second over the last ${rates.length} polls` }, plot);
  const showHover = () => {
    box.querySelectorAll('.spark-dot, .spark-tip').forEach((node) => node.remove());
    if (hover == null || !rates.length) return;
    // The inverse of x(), clamped to the samples there are.
    const index = Math.min(rates.length - 1, Math.max(0, Math.round(hover * width) - width + rates.length - 1));
    const sample = samples[index];
    const left = `${(x(index) / width) * 100}%`;
    const dot = h('span', { class: 'spark-dot' });
    dot.style.left = left;
    dot.style.top = `${(y(sample.rate) / height) * 100}%`;
    const tip = h('span', { class: 'spark-tip' }, `${sample.rate.toFixed(1)}/s · ${clock.format(sample.at)}`);
    tip.style.left = left;
    box.append(dot, tip);
  };
  box.addEventListener('pointermove', (event) => {
    const rect = box.getBoundingClientRect();
    hover = Math.min(1, Math.max(0, (event.clientX - rect.left) / rect.width));
    showHover();
  });
  box.addEventListener('pointerleave', () => {
    hover = null;
    showHover();
  });
  showHover();
  return box;
}

function renderTiles(data, now) {
  const tiles = document.getElementById('tiles');
  const db = data.database;
  const pool = data.server.pool;
  if (!db) {
    tiles.replaceChildren(
      tile('Server uptime', duration(since(data.server.started_at, now)), `Revision ${data.server.revision.slice(0, 7)}`),
      tile('Connection pool', `${pool.size}`, `${pool.idle} idle of ${pool.max} max`),
    );
    return;
  }

  const usage = db.open_connections / db.max_connections;
  const meter = h('div', { class: 'meter', 'data-level': usage >= 0.9 ? 'serious' : usage >= 0.75 ? 'warning' : null, role: 'meter', 'aria-valuemin': 0, 'aria-valuemax': db.max_connections, 'aria-valuenow': db.open_connections, 'aria-label': 'Connections used' }, h('div', { class: 'meter-fill' }));
  meter.firstChild.style.width = `${Math.min(100, usage * 100)}%`;

  const blocks = db.blocks_hit + db.blocks_read;
  const rows = db.tables.reduce((sum, table) => sum + table.rows, 0);
  const entries = db.leaderboards.reduce((sum, board) => sum + board.players, 0);
  const lastPut = db.leaderboards.map((board) => board.last_put).sort().at(-1);
  const rate = samples.at(-1);

  tiles.replaceChildren(
    tile('PostgreSQL uptime', duration(since(db.started_at, now)), `Since ${dateTime.format(new Date(db.started_at))}`),
    tile('Database size', bytes(db.size_bytes), `${db.tables.length} tables · ≈${count(rows)} rows`),
    tile('Connections', [`${db.open_connections}`, h('small', null, `/ ${db.max_connections}`)], `${db.active_connections} active · pool ${pool.size}/${pool.max}`, meter),
    tile('Cache hit ratio', blocks ? percent.format(db.blocks_hit / blocks) : '—', `${count(db.blocks_read)} blocks read from disk`),
    tile('Transactions', rate ? [rate.rate.toFixed(1), h('small', null, '/s')] : '—', `${count(db.commits)} commits · ${count(db.rollbacks)} rollbacks`, sparkline()),
    tile('Leaderboard entries', count(entries), lastPut ? ['Last score ', ago(lastPut, now)] : 'No scores yet'),
  );
}

function renderTables(data, now) {
  const db = data.database;
  const missing = (text) => h('p', { class: 'empty' }, text);
  if (!db) {
    for (const id of ['leaderboards', 'connections', 'tables', 'migrations']) {
      document.getElementById(id).replaceChildren(missing('Unavailable while the database is unreachable.'));
    }
    return;
  }

  document.getElementById('leaderboards').replaceChildren(
    table(
      [
        { label: 'Category', cell: (board) => [CATEGORIES[board.category] || `Category ${board.category}`, h('span', { class: 'sub' }, `#${board.category}`)] },
        { label: 'Entries', num: true, cell: (board) => integer.format(board.players) },
        { label: 'JP', num: true, cell: (board) => integer.format(board.jp) },
        { label: 'US', num: true, cell: (board) => integer.format(board.us) },
        { label: 'EU', num: true, cell: (board) => integer.format(board.eu) },
        { label: 'KR', num: true, cell: (board) => integer.format(board.kr) },
        { label: 'Top score', num: true, cell: (board) => integer.format(board.top_score) },
        { label: 'Last score', cell: (board) => ago(board.last_put, now) },
      ],
      db.leaderboards,
      'No scores yet. They appear after the first match played online.',
    ),
  );

  const tone = { active: 'accent', idle: null, 'idle in transaction': 'warning', 'idle in transaction (aborted)': 'critical' };
  document.getElementById('connections').replaceChildren(
    table(
      [
        { label: 'PID', num: true, cell: (c) => `${c.pid}` },
        { label: 'State', cell: (c) => h('span', { class: 'badge', 'data-tone': tone[c.state] }, c.state || 'unknown') },
        { label: 'For', cell: (c) => (c.state_since ? duration(since(c.state_since, now)) : '—') },
        { label: 'Application', cell: (c) => [c.application || '—', h('span', { class: 'sub' }, [c.user, c.database].filter(Boolean).join(' @ '))] },
        { label: 'Client', cell: (c) => c.client || 'local socket' },
        { label: 'Connected', cell: (c) => (c.connected_at ? ago(c.connected_at, now) : '—') },
        { label: 'Waiting for', cell: (c) => c.wait || '—' },
        { label: 'Query', class: 'query', cell: (c) => (c.query ? h('code', { title: c.query }, c.query) : '—') },
      ],
      db.connections,
      'No other connections.',
    ),
  );

  document.getElementById('tables').replaceChildren(
    table(
      [
        { label: 'Table', cell: (t) => h('code', null, t.name) },
        { label: 'Rows', num: true, cell: (t) => integer.format(t.rows) },
        { label: 'Size', num: true, cell: (t) => bytes(t.total_bytes) },
        { label: 'Indexes', num: true, cell: (t) => bytes(t.index_bytes) },
        { label: 'Dead rows', num: true, cell: (t) => integer.format(t.dead_rows) },
        { label: 'Seq. scans', num: true, cell: (t) => count(t.seq_scans) },
        { label: 'Index scans', num: true, cell: (t) => (t.index_scans == null ? '—' : count(t.index_scans)) },
        { label: 'Vacuumed', cell: (t) => ago(t.last_vacuum, now) },
        { label: 'Analyzed', cell: (t) => ago(t.last_analyze, now) },
      ],
      db.tables,
      'No tables.',
    ),
  );

  document.getElementById('migrations').replaceChildren(
    table(
      [
        { label: 'Version', cell: (m) => h('code', null, `${m.version}`) },
        { label: 'Description', cell: (m) => m.description },
        { label: 'Applied', cell: (m) => ago(m.installed_on, now) },
        { label: 'Took', num: true, cell: (m) => (m.execution_ms < 1 ? '<1 ms' : `${integer.format(m.execution_ms)} ms`) },
        { label: 'Status', cell: (m) => h('span', { class: 'badge', 'data-tone': m.success ? 'good' : 'critical' }, m.success ? 'Applied' : 'Failed') },
      ],
      db.migrations,
      'No migrations applied.',
    ),
  );
}

function renderViewer(viewer) {
  const label = viewer.name || viewer.email;
  const initials = label
    .split(/[\s@.]+/)
    .filter(Boolean)
    .slice(0, 2)
    .map((part) => part[0].toUpperCase())
    .join('');
  const avatar = h('span', { class: 'avatar', title: viewer.email }, initials);
  if (viewer.picture) {
    const image = h('img', { src: viewer.picture, alt: '', referrerpolicy: 'no-referrer' });
    image.addEventListener('error', () => avatar.replaceChildren(initials));
    avatar.replaceChildren(image);
  }
  document.getElementById('viewer').replaceChildren(avatar, h('span', { class: 'viewer-email' }, viewer.email));
}

function renderFooter(data, now) {
  const parts = [
    ['Revision ', h('code', { title: data.server.revision }, data.server.revision.slice(0, 12))],
    `Server up ${duration(since(data.server.started_at, now))}`,
  ];
  if (data.database) parts.push(`PostgreSQL ${data.database.version}`);
  document.getElementById('footer').replaceChildren(...parts.flatMap((part, index) => (index ? [' · ', part] : [part])).flat());
}

function render(data) {
  const now = Date.parse(data.now);
  renderViewer(data.viewer);
  renderStatus(data, now);
  renderTiles(data, now);
  renderTables(data, now);
  renderFooter(data, now);
}

// --- Polling -------------------------------------------------------------

/** Turns the transaction counters into a rate since the last poll. */
function sample(data) {
  const db = data.database;
  if (!db) return;
  const at = new Date(data.now);
  const count = db.commits + db.rollbacks;
  if (previous && count >= previous.count && at > previous.at) {
    samples.push({ at, rate: (count - previous.count) / ((at - previous.at) / 1000) });
    samples = samples.slice(-HISTORY);
  }
  previous = { at, count };
}

function setLive(state, text) {
  const live = document.getElementById('live');
  live.dataset.state = state;
  document.getElementById('live-text').textContent = text;
}

async function poll() {
  clearTimeout(timer);
  try {
    const response = await fetch('/admin/api/overview', { cache: 'no-store', headers: { Accept: 'application/json' } });
    if (response.status === 401) {
      // The session ended: the page itself now shows the sign-in.
      window.location.reload();
      return;
    }
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const data = await response.json();
    sample(data);
    render(data);
    setLive('live', `Live · ${clock.format(new Date(data.now))}`);
  } catch (error) {
    setLive('error', 'Server unreachable · retrying');
  }
  if (!document.hidden) timer = setTimeout(poll, POLL_MS);
}

document.addEventListener('visibilitychange', () => {
  if (document.hidden) {
    clearTimeout(timer);
    setLive('paused', 'Paused');
  } else {
    poll();
  }
});

poll();
