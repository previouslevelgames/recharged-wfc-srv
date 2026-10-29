// The admin dashboard. Every page under /admin is this script: it shows the
// section for its path, polls that section's JSON and renders it. Everything
// is built with DOM calls, so values from the server are never parsed as
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
// The boards that start every day; the others start a season every week
// (docs/gamestats2.md, "Seasons").
const DAILY_BOARDS = new Set([1, 4]);
// DWCRnkRegion.
const REGIONS = { 1: 'JP', 2: 'US', 4: 'EU', 8: 'KR' };
// DWC's status in GPCM's \status\ (docs/login.md, "Mutual friends"), with
// its badge tone.
const STATUSES = {
  0: ['Offline', null],
  1: ['Online', 'good'],
  2: ['Playing', 'accent'],
  3: ['Matchmaking', 'warning'],
  4: ['Matchmaking with friends', 'warning'],
  5: ['Joining a group', 'warning'],
  6: ['Hosting a group', 'warning'],
};
// DWC's dwc_mtype in a session's heartbeat (dwc_match.h), with its badge
// tone. A session without one is not matchmaking.
const MATCH_TYPES = {
  0: ['Ranked search', 'warning'],
  1: ['Searching friends', 'warning'],
  2: ['Hosting friends', 'accent'],
  3: ['Joining a friend', 'accent'],
};
// DWC's match commands (dwc_match.h), with their badge tone.
const MATCH_COMMANDS = {
  0x01: ['Reservation', 'accent'],
  0x02: ['Reservation accepted', 'good'],
  0x03: ['Reservation declined', 'serious'],
  0x04: ['Asked to wait', 'warning'],
  0x05: ['Reservation cancelled', 'serious'],
  0x06: ['Address sent', 'accent'],
  0x07: ['New player', 'accent'],
  0x08: ['Close requested', null],
  0x09: ['Closed', null],
  0x0a: ['Connection closed', null],
  0x0b: ['Prior reservation', 'accent'],
  0x0c: ['Cancel', 'serious'],
  0x0d: ['Cancel sync', null],
  0x0e: ['Cancel sync ack', null],
  0x0f: ['Cancel ack', null],
  0x10: ['Host closed a guest', null],
  0x11: ['Poll timeout', 'warning'],
  0x12: ['Poll timeout ack', null],
  0x13: ['Host blocks guests', 'warning'],
  0x20: ['Friend accepted', 'good'],
  0x40: ['Guest waiting', null],
  0x41: ['Host keepalive', null],
};
const SEVERITY = ['good', 'warning', 'serious', 'critical'];
// tracing's levels, as the log files write them, with their badge tone.
const LEVELS = {
  ERROR: ['Error', 'critical'],
  WARN: ['Warning', 'warning'],
  INFO: ['Info', null],
  DEBUG: ['Debug', null],
  TRACE: ['Trace', null],
};
// What a rule set can hold (src/profiles/rules.rs), with the names players
// know.
const TWEAK_NAMES = {
  fast_terrain: 'Fast terrain',
  no_mega_strikes: 'No Mega Strikes',
  thunder_island_walls: 'Thunder Island walls',
};
const SKILL_NAMES = {
  training: 'Training',
  rookie: 'Rookie',
  professional: 'Professional',
  superstar: 'Superstar',
  legend: 'Legend',
};
const STADIUM_NAMES = {
  random: 'Random (the host picks)',
  battle_dome: 'The Battle Dome',
  bowser_stadium: 'Bowser Stadium',
  crater_field: 'Crater Field',
  crystal_canyon: 'Crystal Canyon',
  the_dump: 'The Dump',
  galactic_stadium: 'Galactic Stadium',
  konga_coliseum: 'Konga Coliseum',
  lava_pit: 'Lava Pit',
  pipeline_central: 'Pipeline Central',
  storm_ship: 'Storm Ship',
  the_palace: 'The Palace',
  thunder_island: 'Thunder Island',
  the_underground: 'The Underground',
  the_vice: 'The Vice',
  the_wastelands: 'The Wastelands',
  sand_tomb: 'Sand Tomb',
  unknown: 'Unknown',
};
// The captains and sidekicks a match report names (src/reports/report.rs).
const CAPTAIN_NAMES = {
  mario: 'Mario',
  bowser: 'Bowser',
  daisy: 'Daisy',
  donkey_kong: 'Donkey Kong',
  luigi: 'Luigi',
  peach: 'Peach',
  waluigi: 'Waluigi',
  wario: 'Wario',
  yoshi: 'Yoshi',
  bowser_jr: 'Bowser Jr.',
  diddy_kong: 'Diddy Kong',
  petey: 'Petey Piranha',
  unknown: 'Unknown',
};
const SIDEKICK_NAMES = {
  toad: 'Toad',
  koopa: 'Koopa',
  hammer_bro: 'Hammer Bro',
  birdo: 'Birdo',
  boo: 'Boo',
  dry_bones: 'Dry Bones',
  monty_mole: 'Monty Mole',
  shy_guy: 'Shy Guy',
};
// An online match's state (docs/reports.md, "Games and states"), with its
// badge tone.
const ONLINE_STATES = {
  in_progress: ['In progress', 'accent'],
  finished: ['Finished', 'good'],
  disputed: ['Disputed', 'warning'],
  abandoned: ['Abandoned', null],
};
// A game of an online match: whether its sides saw it the same.
const GAME_STATUSES = {
  agreed: ['Agreed', 'good'],
  disputed: ['Disputed', 'warning'],
  only_home: ['Home only', null],
  only_away: ['Away only', null],
};
const MODES = { ranked: 'Ranked', friends: 'Friends' };
const ENDINGS = { normal: 'Normal', disconnect: 'Disconnect', desync: 'Desync' };
// Why a series ended, as a Recharged build's end says (docs/reports.md,
// "Diagnostics"), with its badge tone.
const CAUSES = {
  decided: ['Decided', 'good'],
  desync: ['Out of sync', 'serious'],
  disconnect: ['Lost connection', 'warning'],
  offline: ['Went offline', null],
};
// The game's sync checks: the first one that failed in a desync.
const SYNC_CHECKS = { frame: 'Frame mismatch', checksum: 'Checksum mismatch', seed: 'Random seed mismatch' };
// How a game's GPCM login ended while its match was in progress.
const LEFT_HOW = {
  logout: 'logged out of WFC',
  closed: 'closed without logging out',
  lost: 'connection broke off',
  replaced: 'another login took over',
};
// A Monitoring incident, with its badge tone.
const INCIDENTS = {
  desync: ['Out of sync', 'serious'],
  disconnect: ['Lost connection', 'warning'],
  left: ['Left WFC', 'warning'],
  offline: ['Went offline', null],
  abandoned: ['Abandoned', null],
};
// DWC's error codes (dwc_error.h): 86420 is the matchmaking sequence (8),
// NAT negotiation (6), repeated failures (420).
const DWC_SEQUENCES = { 6: 'Login', 7: 'Friends', 8: 'Matchmaking', 9: 'Other' };
const DWC_PARTS = {
  1: 'GPCM (presence)',
  2: 'Persistent storage',
  4: 'QR2 (master server)',
  5: 'Server browser',
  6: 'NAT negotiation',
  7: 'Peer connection',
};
const DWC_KINDS = {
  1: 'out of memory',
  2: 'bad parameter',
  3: 'socket',
  9: 'unexpected',
  10: 'network',
  20: 'server',
  30: 'DNS',
  40: 'bad data',
  50: 'socket',
  60: 'bind',
  70: 'timeout',
  80: 'peer',
  100: 'too many connections',
  200: 'stats sign-in',
  210: 'stats load',
  220: 'stats save',
  410: 'other',
  420: 'repeated failures',
  430: 'server or client failure',
  600: 'closed',
  610: 'transfer header',
  620: 'transfer body',
};
// The game's session state when an error came (NetworkSession::mSessionState).
const PHASES = {
  none: 'Menus',
  login: 'Login',
  matchmaking: 'Matchmaking',
  prestart: 'Before kickoff',
  loading: 'Loading',
  in_game: 'In a game',
  game_end: 'After a game',
};
// Why a report was refused (src/reports/mod.rs).
const REFUSALS = { elsewhere: 'From another address', invalid: 'Invalid report' };
// A match's state (docs/profiles.md, "Matches"), with its badge tone.
const MATCH_STATES = {
  challenged: ['Challenged', 'warning'],
  accepted: ['Accepted', 'accent'],
  finished: ['Finished', 'good'],
  disputed: ['Disputed', 'serious'],
  declined: ['Declined', null],
  cancelled: ['Cancelled', null],
  expired: ['Expired', null],
  dissolved: ['Dissolved', null],
};

const ICONS = {
  overview: 'M3 3h7v7H3zM14 3h7v7h-7zM14 14h7v7h-7zM3 14h7v7H3z',
  players: 'M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2M9 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM22 21v-2a4 4 0 0 0-3-3.87M16 3.13a4 4 0 0 1 0 7.75',
  matchmaking: 'M5 12.55a11 11 0 0 1 14.08 0M1.42 9a16 16 0 0 1 21.16 0M8.53 16.11a6 6 0 0 1 6.95 0M12 20h.01',
  matches: 'M7 16V4M3 8l4-4 4 4M17 8v12M21 16l-4 4-4-4',
  monitoring: 'M22 12h-4l-3 9L9 3l-3 9H2',
  leaderboards: 'M6 9H4.5a2.5 2.5 0 0 1 0-5H6M18 9h1.5a2.5 2.5 0 0 0 0-5H18M4 22h16M10 14.66V17c0 .55-.47.98-.97 1.21C7.85 18.75 7 20.24 7 22M14 14.66V17c0 .55.47.98.97 1.21C16.15 18.75 17 20.24 17 22M18 2H6v7a6 6 0 0 0 12 0V2z',
  competitive: 'M4 22v-7M4 15V3h13l-3 6 3 6H4',
  moderation: 'M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z',
  database: 'M12 8c4.97 0 9-1.34 9-3s-4.03-3-9-3-9 1.34-9 3 4.03 3 9 3zM21 12c0 1.66-4 3-9 3s-9-1.34-9-3M3 5v14c0 1.66 4 3 9 3s9-1.34 9-3V5',
  logs: 'M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8zM14 2v6h6M16 13H8M16 17H8M10 9H8',
  maintenance: 'M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z',
  good: 'M20 6 9 17l-5-5',
  warning: 'M12 9v4m0 4h.01M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z',
  serious: 'M12 9v4m0 4h.01M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z',
  critical: 'M18 6 6 18M6 6l12 12',
  pending: 'M12 6v6l4 2',
};

// The sections, in the order of the server's PAGES (src/admin/mod.rs). A
// planned section has no data of its own yet: it polls the overview, for
// the header and footer. An owner's section, in the server's OWNER_PAGES,
// shows to owners only.
const SECTIONS = [
  {
    path: '/admin',
    name: 'Overview',
    icon: 'overview',
    intro: 'Every part of the WFC server at a glance.',
    api: 'overview',
    render: renderOverview,
  },
  {
    path: '/admin/players',
    group: 'Game',
    name: 'Players',
    icon: 'players',
    intro: 'The consoles that registered, their profiles with friend codes, and who is online.',
    api: 'players',
    render: renderPlayers,
    detail: renderPlayer,
  },
  {
    path: '/admin/matchmaking',
    group: 'Game',
    name: 'Matchmaking',
    icon: 'matchmaking',
    intro: 'The games online, and what each is doing in matchmaking.',
    api: 'matchmaking',
    render: renderMatchmaking,
  },
  {
    path: '/admin/matches',
    group: 'Game',
    name: 'Matches',
    icon: 'matches',
    intro: 'The series Recharged builds play online, ranked and with friends, as both games reported them.',
    api: 'matches',
    render: renderMatches,
    detail: renderMatch,
  },
  {
    path: '/admin/leaderboards',
    group: 'Game',
    name: 'Leaderboards',
    icon: 'leaderboards',
    intro: 'gamestats2: the scores players put, and who leads each board.',
    api: 'leaderboards',
    render: renderLeaderboards,
  },
  {
    path: '/admin/competitive',
    group: 'Game',
    name: 'Competitive',
    icon: 'competitive',
    intro: 'Recharged profiles: the rules matches are played by, the players who signed in from the launcher, and their matches.',
    api: 'competitive',
    render: renderCompetitive,
    editor: true,
  },
  {
    path: '/admin/moderation',
    group: 'Game',
    name: 'Moderation',
    icon: 'moderation',
    intro: 'Keep online play fair.',
    planned: {
      items: [
        'Ban a profile or a console, with a reason and an end date',
        'Kick a player out of a match',
        'Remove a leaderboard score',
      ],
      needs: 'the Players and Matchmaking sections',
    },
  },
  {
    path: '/admin/monitoring',
    group: 'System',
    name: 'Monitoring',
    icon: 'monitoring',
    intro: 'How online matches go wrong: games out of sync, lost connections, games that left WFC mid-match, and reports the server refused.',
    api: 'monitoring',
    render: renderMonitoring,
  },
  {
    path: '/admin/database',
    group: 'System',
    name: 'Database',
    icon: 'database',
    intro: 'PostgreSQL, where the server keeps its data: health, load and schema.',
    api: 'database',
    render: renderDatabase,
  },
  {
    path: '/admin/logs',
    group: 'System',
    name: 'Logs',
    icon: 'logs',
    intro: "The server's log files, one per day: read them here, or download a whole day.",
    api: 'logs',
    render: renderLogs,
  },
  {
    path: '/admin/maintenance',
    group: 'System',
    name: 'Maintenance',
    icon: 'maintenance',
    intro: "Operations on the server's data that cannot be undone, for owners only. Each one asks twice.",
    api: 'maintenance',
    render: renderMaintenance,
    owner: true,
  },
];

const integer = new Intl.NumberFormat();
const compact = new Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 1 });
// Exact below 10,000, then 12.9K.
const count = (value) => (value < 10000 ? integer : compact).format(value);
const percent = new Intl.NumberFormat(undefined, { style: 'percent', maximumFractionDigits: 1 });
const clock = new Intl.DateTimeFormat(undefined, { timeStyle: 'medium' });
const dateTime = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' });
// Calendar days, as the server sends them ("2026-10-15"): no time zone to shift them.
const calendarDay = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeZone: 'UTC' });
const hourMinute = new Intl.DateTimeFormat(undefined, { timeStyle: 'short' });
const weekdayDay = new Intl.DateTimeFormat(undefined, { weekday: 'short', day: 'numeric' });

// The page for this path: a section, or a row of one that opens a page of
// its own (/admin/matches/12).
const route = (() => {
  const path = window.location.pathname;
  const found = SECTIONS.find((candidate) => candidate.path === path);
  if (found) return { section: found, id: null };
  const row = path.match(/^(\/admin\/[a-z]+)\/(\d+)$/);
  const parent = row && SECTIONS.find((candidate) => candidate.path === row[1] && candidate.detail);
  return parent ? { section: parent, id: row[2] } : { section: SECTIONS[0], id: null };
})();
const section = route.section;

let timer = null;
let lastData = null; // For re-rendering on a click, between polls.
let previous = null; // The last transaction counters.
let samples = []; // { at, rate } for the sparkline.
let hover = null; // The pointer's position across the sparkline, 0 to 1.
let category = 0; // The board whose leaders are shown.
// The Monitoring timeline across polls: the column picked by the pointer or
// the keyboard (its start, in ms), whether the chart has the keyboard focus,
// and whether its table is open.
const timelineView = { column: null, focused: false, table: false };
let polls = 0; // Polls started, so a slow answer never replaces a newer one.

// --- DOM helpers ---------------------------------------------------------

function h(tag, props, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props || {})) {
    if (value == null || value === false) continue;
    if (key === 'class') node.className = value;
    // The pages' policy (style-src 'self') drops style attributes: styles
    // are an object of properties, set through the CSSOM.
    else if (key === 'style') Object.assign(node.style, value);
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

function icon(name) {
  return svg(
    'svg',
    { viewBox: '0 0 24 24', fill: 'none', stroke: 'currentColor', 'stroke-width': '2', 'stroke-linecap': 'round', 'stroke-linejoin': 'round', 'aria-hidden': 'true' },
    svg('path', { d: ICONS[name] || ICONS.pending }),
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

// --- Building blocks -----------------------------------------------------

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

/** A card holding a table, with an optional row of controls above it. */
function card(title, note, body, tools) {
  return h(
    'section',
    { class: 'card' },
    h('header', { class: 'card-head' }, h('h2', null, title), note && h('p', null, note)),
    tools && h('div', { class: 'card-tools' }, tools),
    h('div', { class: 'table-wrap' }, body),
  );
}

function banner({ level, title, detail }) {
  return h(
    'section',
    { class: 'status', 'data-level': level, 'aria-live': 'polite' },
    h('span', { class: 'status-icon' }, icon(level)),
    h('div', { class: 'status-text' }, h('strong', null, title), h('span', null, detail)),
  );
}

function unreachable(error) {
  return banner({ level: 'critical', title: 'Database unreachable', detail: error || 'No answer.' });
}

function tile(label, value, sub, ...extra) {
  return h(
    'article',
    { class: 'tile' },
    h('span', { class: 'tile-label' }, label),
    h('span', { class: 'tile-value' }, value),
    ...extra,
    h('span', { class: 'tile-sub' }, sub),
  );
}

/** Sends a change to the server. Returns its JSON answer, or throws with
 * the error it gives. */
async function send(method, path, body) {
  const response = await fetch(path, {
    method,
    headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
    body: body && JSON.stringify(body),
  });
  if (response.status === 401) window.location.reload();
  const answer = response.status === 204 ? null : await response.json().catch(() => null);
  if (!response.ok) throw new Error((answer && answer.error) || `HTTP ${response.status}`);
  return answer;
}

function badge(tone, text) {
  return h('span', { class: 'badge', 'data-tone': tone }, text);
}

/** How the database is doing, from its summary. */
function databaseHealth(db, now) {
  const issues = [];
  const usage = db.open_connections / db.max_connections;
  if (usage >= 0.9) issues.push(['serious', `Connections at ${percent.format(usage)} of max_connections`]);
  else if (usage >= 0.75) issues.push(['warning', `Connections at ${percent.format(usage)} of max_connections`]);
  const about = `PostgreSQL ${shortVersion(db.version)} · ${db.name} · up ${duration(since(db.started_at, now))}`;
  if (!issues.length) return { level: 'good', title: 'Database healthy', detail: about };
  issues.sort((a, b) => SEVERITY.indexOf(b[0]) - SEVERITY.indexOf(a[0]));
  return { level: issues[0][0], title: issues[0][1], detail: issues.slice(1).map((issue) => issue[1]).join(' · ') || about };
}

// --- Overview ------------------------------------------------------------

function serviceCard(target, { tone, status, facts, text, foot }) {
  return h(
    'a',
    { class: 'service', href: target.path, 'data-planned': target.planned ? 'true' : null },
    h(
      'div',
      { class: 'service-head' },
      h('span', { class: 'service-icon' }, icon(target.icon)),
      h('strong', null, target.name),
      badge(tone, status),
    ),
    text && h('p', { class: 'service-text' }, text),
    facts && h('dl', { class: 'service-facts' }, facts.map(([label, value]) => h('div', null, h('dt', null, label), h('dd', null, value)))),
    foot && h('p', { class: 'service-foot' }, foot),
  );
}

function renderOverview(data, now) {
  const content = data.content;
  const sections = SECTIONS.filter((target) => !target.owner || data.viewer.owner);
  const planned = sections.filter((target) => target.planned).length;
  const live = sections.length - 1 - planned;
  let status;
  if (!content) {
    status = { level: 'critical', title: 'Database unreachable', detail: `The leaderboards are down with it. ${data.error || ''}` };
  } else {
    const health = databaseHealth(content.database, now);
    status = health.level === 'good'
      ? { level: 'good', title: 'Everything live is running', detail: `${live} sections live · ${planned} planned` }
      : health;
  }

  // Live sections first, then the planned ones.
  const targets = sections.filter((target) => target.path !== '/admin');
  const ordered = [...targets.filter((target) => !target.planned), ...targets.filter((target) => target.planned)];
  const cards = ordered.map((target) => {
    if (target.planned) {
      return serviceCard(target, { tone: null, status: 'Planned', text: target.intro, foot: `Needs ${target.planned.needs}` });
    }
    // Files on the server's disk, there without the database too.
    if (target.path === '/admin/logs') {
      return serviceCard(target, {
        tone: null,
        status: 'Daily files',
        text: content ? target.intro : 'The database does not answer: the logs say why.',
        foot: 'One file per day (UTC), in LOG_DIR',
      });
    }
    if (!content) return serviceCard(target, { tone: 'critical', status: 'Down', text: 'The database does not answer.' });
    if (target.owner) return serviceCard(target, { tone: null, status: 'Owners only', text: target.intro });
    if (target.path === '/admin/database') {
      const db = content.database;
      return serviceCard(target, {
        tone: 'good',
        status: 'Running',
        facts: [
          ['Version', shortVersion(db.version)],
          ['Size', bytes(db.size_bytes)],
          ['Connections', `${db.open_connections} / ${db.max_connections}`],
        ],
        foot: `PostgreSQL · up ${duration(since(db.started_at, now))}`,
      });
    }
    if (target.path === '/admin/players') {
      const players = content.players;
      return serviceCard(target, {
        tone: 'good',
        status: 'Live',
        facts: [
          ['Online', count(players.online)],
          ['Profiles', count(players.profiles)],
          ['Last sign-in', players.last_login ? ago(players.last_login, now) : 'none yet'],
        ],
        foot: `NAS and GPCM · ${count(players.accounts)} accounts`,
      });
    }
    if (target.path === '/admin/matchmaking') {
      const games = content.matchmaking;
      return serviceCard(target, {
        tone: 'good',
        status: 'Live',
        facts: [
          ['Sessions', count(games.sessions)],
          ['Ranked search', count(games.searching)],
          ['With friends', count(games.with_friends)],
        ],
        foot: 'QR2, server browser and NAT negotiation',
      });
    }
    if (target.path === '/admin/matches') {
      const matches = content.matches;
      return serviceCard(target, {
        tone: 'good',
        status: 'Live',
        facts: [
          ['In progress', count(matches.in_progress)],
          ['Matches', count(matches.matches)],
          ['Last report', matches.last_report ? ago(matches.last_report, now) : 'none yet'],
        ],
        foot: `Recharged's match reports · ${count(matches.disputed)} disputed`,
      });
    }
    if (target.path === '/admin/leaderboards') {
      const boards = content.leaderboards;
      return serviceCard(target, {
        tone: 'good',
        status: 'Live',
        facts: [
          ['Entries', count(boards.entries)],
          ['Players', count(boards.players)],
          ['Last score', boards.last_put ? ago(boards.last_put, now) : 'none yet'],
        ],
        foot: 'gamestats2 · get2.asp and put2.asp',
      });
    }
    return serviceCard(target, { tone: 'good', status: 'Live', text: target.intro });
  });

  return [banner(status), h('section', { class: 'services', 'aria-label': 'Sections' }, cards)];
}

// --- Players -------------------------------------------------------------

function presence(player, now) {
  if (player.cpu_buddy) return [badge('accent', 'Server'), h('span', { class: 'sub' }, 'Accepts every request')];
  if (!player.online) return badge(null, 'Offline');
  const [name, tone] = STATUSES[player.online.status] || ['Logged in', 'good'];
  // A status counts from when the game sent it, the login from its start.
  const start = player.online.status_since || player.online.since;
  return [badge(tone, name), h('span', { class: 'sub' }, ['since ', ago(start, now)])];
}

function renderPlayers(data, now) {
  const content = data.content;
  if (!content) return [unreachable(data.error)];
  const { totals, profiles } = content;
  const shown = profiles.filter((player) => !player.cpu_buddy).length;

  return [
    h(
      'section',
      { class: 'tiles', 'aria-label': 'Key figures' },
      tile('Online', count(totals.online), 'Logged in to GPCM now'),
      tile('Profiles', count(totals.profiles), 'One per console and Mii'),
      tile('Accounts', count(totals.accounts), 'Consoles registered with NAS'),
      tile('Last sign-in', totals.last_login ? ago(totals.last_login, now) : '—', totals.last_login ? dateTime.format(new Date(totals.last_login)) : 'Nobody yet'),
    ),
    card(
      'Players',
      shown < totals.profiles
        ? `The ${integer.format(shown)} profiles that signed in last, online first, of ${integer.format(totals.profiles)}`
        : 'Every profile, online first; the CPU buddy is the server\'s own',
      table(
        [
          { label: 'Player', cell: (p) => [h('a', { href: playerPath(p.profile_id) }, p.mii_name || 'Unnamed'), h('span', { class: 'sub' }, `Profile ${p.profile_id}`)] },
          { label: 'Friend code', cell: (p) => h('code', null, p.friend_code) },
          { label: 'Status', cell: (p) => presence(p, now) },
          {
            label: 'Friends',
            num: true,
            cell: (p) => (p.online ? [`${p.online.mutual_friends}`, h('span', { class: 'sub' }, `of ${p.online.friend_requests} asked`)] : '—'),
          },
          { label: 'Last sign-in', cell: (p) => (p.cpu_buddy ? '—' : ago(p.last_login_at, now)) },
          { label: 'Console', cell: (p) => [h('code', null, `${p.user_id}`), h('span', { class: 'sub' }, p.gsbrcd)] },
          { label: 'Address', cell: (p) => p.last_ip || '—' },
          { label: 'Scores', num: true, cell: (p) => integer.format(p.scores) },
          { label: 'Joined', cell: (p) => ago(p.created_at, now) },
        ],
        profiles,
        'No players yet. A console appears here after its first sign-in.',
      ),
    ),
  ];
}

// --- A player's page -----------------------------------------------------

function record(won, lost, drawn) {
  return `${integer.format(won)}–${integer.format(lost)}–${integer.format(drawn)}`;
}

function playedResult(match, now) {
  if (match.result) {
    const [name, tone] = { won: ['Won', 'good'], lost: ['Lost', 'critical'], drawn: ['Drawn', null] }[match.result];
    return [badge(tone, name), h('span', { class: 'sub' }, ago(match.ended_at || match.started_at, now))];
  }
  return onlineState({ ...match, last_report_at: match.started_at }, now);
}

function renderPlayer(data, now) {
  const detail = data.content;
  if (!detail) {
    if (data.error) return [unreachable(data.error)];
    return [banner({ level: 'warning', title: 'No such profile', detail: `No profile ${route.id} has signed in.` })];
  }
  const { profile, stats, standing, matches } = detail;
  setHead(profile.mii_name || 'Unnamed', [`Profile ${profile.profile_id} · friend code `, h('code', null, profile.friend_code)]);
  const player = profile.player;

  return [
    h(
      'section',
      { class: 'tiles', 'aria-label': 'Key figures' },
      tile(
        'Status',
        profile.cpu_buddy ? 'Server' : profile.online ? (STATUSES[profile.online.status] || ['Logged in'])[0] : 'Offline',
        !profile.online
          ? 'Not logged in to GPCM'
          : profile.online.status_since
            ? ['Since ', ago(profile.online.status_since, now), ' · logged in to GPCM ', ago(profile.online.since, now)]
            : ['Logged in to GPCM ', ago(profile.online.since, now)],
      ),
      tile('Release', profile.release, profile.gsbrcd),
      tile('Account', h('code', null, `${profile.user_id}`), 'The console, registered with NAS'),
      tile('Last sign-in', profile.cpu_buddy ? '—' : ago(profile.last_login_at, now), profile.last_ip || 'No address'),
      tile('Recharged player', player ? player.name : '—', player ? (player.discord ? `Discord ${player.discord}` : 'Test player') : 'Not signed in from the launcher'),
    ),
    card(
      'Record',
      'The series its Recharged build reported, by mode; a game counts when both sides saw it the same, or only one reported it',
      table(
        [
          { label: 'Mode', cell: (m) => MODES[m.mode] || m.mode },
          { label: 'Series', num: true, cell: (m) => integer.format(m.series.played) },
          { label: 'Won–lost–drawn', num: true, cell: (m) => record(m.series.won, m.series.lost, m.series.drawn) },
          {
            label: 'Other series',
            cell: (m) => h('span', { class: 'sub' }, `${m.series.in_progress} in progress · ${m.series.disputed} disputed · ${m.series.abandoned} abandoned`),
          },
          { label: 'Games', num: true, cell: (m) => integer.format(m.games.played) },
          { label: 'Won–lost–drawn', num: true, cell: (m) => record(m.games.won, m.games.lost, m.games.drawn) },
          { label: 'Goals', num: true, cell: (m) => [`${integer.format(m.games.goals_for)} : ${integer.format(m.games.goals_against)}`, h('span', { class: 'sub' }, 'for : against')] },
          {
            label: 'Captains',
            class: 'wrap',
            cell: (m) => (m.captains.length ? m.captains.map((c) => `${captainName(c.captain)} ×${c.games}`).join(', ') : h('span', { class: 'sub' }, 'none')),
          },
        ],
        stats,
        'No modes.',
      ),
    ),
    card(
      'Ranked standing',
      'Its rows on the leaderboards, ranked as on the Leaderboards page',
      table(
        [
          { label: 'Board', cell: (e) => CATEGORIES[e.category] || `Category ${e.category}` },
          { label: 'Rank', num: true, cell: (e) => [`${e.rank}`, h('span', { class: 'sub' }, `of ${integer.format(e.entries)}`)] },
          { label: 'Score', num: true, cell: (e) => integer.format(e.score) },
          { label: 'Wins', num: true, cell: (e) => (e.wins == null ? '—' : integer.format(e.wins)) },
          { label: 'Losses', num: true, cell: (e) => (e.losses == null ? '—' : integer.format(e.losses)) },
          { label: 'Updated', cell: (e) => ago(e.updated_at, now) },
        ],
        standing,
        'No scores on the leaderboards.',
      ),
    ),
    card(
      'Matches',
      'The series its game reported, the newest first',
      table(
        [
          { label: 'Started', cell: (m) => [ago(m.started_at, now), h('span', { class: 'sub' }, dateTime.format(new Date(m.started_at)))] },
          { label: 'Mode', cell: (m) => [MODES[m.mode] || m.mode, h('span', { class: 'sub' }, `${m.release} · best of ${m.best_of}`)] },
          { label: 'Played as', cell: (m) => [captainName(m.captain), h('span', { class: 'sub' }, m.side === 'home' ? 'Home' : 'Away')] },
          { label: 'Games', num: true, cell: (m) => h('a', { href: matchPath(m) }, `${m.games_won} – ${m.games_lost}`) },
          { label: 'Opponent', cell: (m) => [miiLink(m.opponent.mii, m.opponent.profile_id), h('span', { class: 'sub' }, captainName(m.opponent.captain))] },
          { label: 'Result', cell: (m) => playedResult(m, now) },
          { label: '', class: 'actions', cell: (m) => h('a', { class: 'button', href: matchPath(m) }, 'Open') },
        ],
        matches,
        'No matches reported yet. They appear here when its Recharged build plays online.',
      ),
    ),
  ];
}

// --- Matchmaking ---------------------------------------------------------

function matchState(game) {
  const [name, tone] = MATCH_TYPES[game.match_type] || ['Online', 'good'];
  let sub;
  if (game.match_type == null) sub = 'Not matchmaking';
  else if (game.reserved_with) sub = `Reserved with ${game.reserved_with}`;
  else sub = `${game.players ?? 0} of ${game.max_players ?? 0} found`;
  return [badge(tone, name), h('span', { class: 'sub' }, sub)];
}

// GT: 1 tournament, 2 two players at one console; CO: the region group.
function matchMode(game) {
  if (game.mode == null) return '—';
  const kinds = [];
  if (game.mode & 1) kinds.push('Tournament');
  if (game.mode & 2) kinds.push('Two players');
  return [kinds.join(', ') || 'Normal', h('span', { class: 'sub' }, `Region group ${game.region_group ?? '—'}`)];
}

// A profile in an event: its Mii name and number.
function party(profile, names) {
  if (profile == null) return h('span', { class: 'sub' }, 'Unknown');
  return [names[profile] || 'Unnamed', h('span', { class: 'sub' }, `Profile ${profile}`)];
}

function eventWhat(event) {
  if (event.kind === 'search') {
    const repeats = event.searches > 1 ? ` · ${integer.format(event.searches)} times` : '';
    return [badge('warning', 'Search'), h('span', { class: 'sub' }, `Found ${event.found}${repeats}`)];
  }
  if (event.kind === 'command') {
    const [name, tone] = MATCH_COMMANDS[event.command] || [`Command 0x${event.command.toString(16)}`, null];
    const via = event.via === 'gpcm' ? 'between friends, GPCM' : 'server browser';
    const repeats = event.count > 1 ? ` · ${integer.format(event.count)} times` : '';
    return [badge(tone, name), h('span', { class: 'sub' }, `${via}${repeats}`)];
  }
  const failed = event.reports.some((report) => report.result !== 1);
  const done = event.reports.length >= 2 && !failed;
  let state;
  if (failed) state = badge('critical', 'NAT negotiation failed');
  else if (done) state = badge('good', 'NAT negotiation ok');
  else state = badge('warning', event.connected ? 'NAT negotiation connecting' : 'NAT negotiation started');
  const relayed = event.relayed ? ' · through the relay' : '';
  return [state, h('span', { class: 'sub' }, `Cookie ${event.cookie}${relayed} · ${event.reports.length} of 2 reports`)];
}

function eventWho(event, names) {
  if (event.kind === 'search') return party(event.profile_id, names);
  if (event.kind === 'command') {
    return h('div', { class: 'pair' }, h('div', null, party(event.from, names)), h('span', { class: 'sub' }, '→'), h('div', null, party(event.to, names)));
  }
  if (!event.games.length) return '—';
  return h(
    'div',
    { class: 'pair' },
    event.games.flatMap((game, index) => [
      index ? h('span', { class: 'sub' }, '↔') : null,
      h('div', null, game.profile_id == null ? [game.address, h('span', { class: 'sub' }, 'No session')] : party(game.profile_id, names)),
    ]),
  );
}

// The game's own part of a search filter, in its parentheses.
function eventDetail(event) {
  if (event.kind !== 'search') return '—';
  const own = event.filter.match(/\((.*)\)\s*$/);
  return h('code', { title: event.filter }, own ? own[1] : event.filter);
}

function renderMatchmaking(data, now) {
  const content = data.content;
  if (!content) return [unreachable(data.error)];
  const { totals, sessions, activity, names } = content;
  const idle = totals.sessions - totals.searching - totals.with_friends;

  return [
    h(
      'section',
      { class: 'tiles', 'aria-label': 'Key figures' },
      tile('Sessions', count(totals.sessions), 'Games sending heartbeats'),
      tile('Ranked search', count(totals.searching), 'Looking for an opponent'),
      tile('With friends', count(totals.with_friends), 'Hosting, joining or searching'),
      tile('Not matchmaking', count(idle), 'Online in the menus'),
    ),
    card(
      'Sessions',
      'One per game, from its last heartbeat; matchmaking first',
      table(
        [
          {
            label: 'Player',
            cell: (g) => (g.profile_id == null
              ? ['—', h('span', { class: 'sub' }, 'No profile until matchmaking')]
              : [g.mii_name || 'Unnamed', h('span', { class: 'sub' }, `Profile ${g.profile_id}`)]),
          },
          { label: 'Doing', cell: (g) => matchState(g) },
          { label: 'Mode', cell: (g) => matchMode(g) },
          { label: 'Points', num: true, cell: (g) => (g.points == null ? '—' : integer.format(g.points)) },
          { label: 'Address', cell: (g) => [g.address, h('span', { class: 'sub' }, g.local_address ? `Local ${g.local_address}` : '')] },
          { label: 'Heard', cell: (g) => [ago(g.heard_at, now), g.listed ? null : h('span', { class: 'sub' }, 'Unchallenged')] },
        ],
        sessions,
        'No game is online. A game appears here with its first heartbeat after it signs in.',
      ),
    ),
    card(
      'Recent activity',
      'The latest searches, match commands and NAT negotiations, kept in memory until the server restarts',
      table(
        [
          {
            label: 'When',
            cell: (e) => {
              const last = ago(e.last_at, now);
              const first = ago(e.first_at, now);
              return [last, first.textContent === last.textContent ? null : h('span', { class: 'sub' }, ['since ', first])];
            },
          },
          { label: 'What', cell: (e) => eventWhat(e) },
          { label: 'Who', cell: (e) => eventWho(e, names) },
          { label: 'Filter', class: 'query', cell: (e) => eventDetail(e) },
        ],
        activity,
        'Nothing yet. Searches, reservations and NAT negotiations appear here as games make them.',
      ),
    ),
  ];
}

// --- Matches -------------------------------------------------------------

function captainName(captain) {
  return CAPTAIN_NAMES[captain] || captain;
}

function onlineState(match, now) {
  const [name, tone] = ONLINE_STATES[match.state] || [match.state, null];
  const sub = match.ended_at ? ['ended ', ago(match.ended_at, now)] : ['last report ', ago(match.last_report_at, now)];
  return [badge(tone, name), h('span', { class: 'sub' }, sub)];
}

function playerPath(profileId) {
  return `/admin/players/${profileId}`;
}

/** A Mii's name, to its profile's page when a game reported it. */
function miiLink(mii, profileId) {
  return profileId == null ? mii : h('a', { href: playerPath(profileId) }, mii);
}

/** A team in the list: its Mii, bold when it won, and its captain. */
function matchTeam(team, won) {
  const link = miiLink(team.mii, team.profile_id);
  const name = won ? h('strong', null, link) : link;
  const reported = team.profile_id == null ? ' · no reports' : '';
  return [name, h('span', { class: 'sub' }, `${captainName(team.captain)}${reported}`)];
}

function matchPath(match) {
  return `/admin/matches/${match.id}`;
}

function renderMatches(data, now) {
  const content = data.content;
  if (!content) return [unreachable(data.error)];
  const { totals, matches } = content;
  const ended = totals.finished + totals.disputed + totals.abandoned;

  return [
    h(
      'section',
      { class: 'tiles', 'aria-label': 'Key figures' },
      tile('In progress', count(totals.in_progress), 'Series being played'),
      tile('Finished', count(totals.finished), `${count(totals.disputed)} disputed · ${count(totals.abandoned)} abandoned`),
      tile('Matches', count(totals.matches), `${count(totals.ranked)} ranked · ${count(totals.matches - totals.ranked)} with friends`),
      tile('One-sided', count(totals.one_sided), 'Only one game reported'),
    ),
    card(
      'Matches',
      matches.length < totals.matches
        ? `In progress first, then the newest: ${integer.format(matches.length)} of ${integer.format(totals.matches)}`
        : `In progress first, then the newest; ${integer.format(ended)} ended`,
      table(
        [
          { label: 'Started', cell: (m) => [ago(m.started_at, now), h('span', { class: 'sub' }, dateTime.format(new Date(m.started_at)))] },
          { label: 'Mode', cell: (m) => [MODES[m.mode] || m.mode, h('span', { class: 'sub' }, `${m.release} · best of ${m.best_of}`)] },
          { label: 'Home', cell: (m) => matchTeam(m.home, m.winner === 'home') },
          { label: 'Score', num: true, cell: (m) => h('a', { href: matchPath(m) }, `${m.score.home} – ${m.score.away}`) },
          { label: 'Away', cell: (m) => matchTeam(m.away, m.winner === 'away') },
          { label: 'State', cell: (m) => onlineState(m, now) },
          { label: '', class: 'actions', cell: (m) => h('a', { class: 'button', href: matchPath(m) }, 'Open') },
        ],
        matches,
        'No matches yet. A match appears here when a Recharged build reports the first kickoff of a series.',
      ),
    ),
  ];
}

/** What a finished match came to, for its banner. */
function matchResult(match) {
  const score = `${match.score.home} – ${match.score.away}`;
  if (!match.winner) return `A draw, ${score}`;
  return `${match.winner === 'home' ? match.home.mii : match.away.mii} won, ${score}`;
}

function matchBanner(match, now, why) {
  const times = match.ended_at
    ? `Ended ${ago(match.ended_at, now).textContent} · took ${duration(since(match.started_at, now) - since(match.ended_at, now))}`
    : `Last report ${ago(match.last_report_at, now).textContent}`;
  if (match.state === 'finished') return banner({ level: 'good', title: matchResult(match), detail: times });
  if (match.state === 'disputed') {
    return banner({ level: 'warning', title: 'Disputed', detail: `The two games saw a game differently · ${times}` });
  }
  if (match.state === 'abandoned') {
    return banner({ level: 'serious', title: 'Abandoned', detail: `${why || 'The series stopped before a side won it'} · ${times}` });
  }
  return banner({ level: 'pending', title: `In progress, ${match.score.home} – ${match.score.away}`, detail: times });
}

/** Why a series stopped, from what its games told and GPCM saw: the first
 * of a desync, a lost connection, a game that went offline or left WFC. */
function stopCause(detail) {
  const views = detail.games.flatMap((game) => [game.home, game.away]).filter(Boolean);
  const desync = detail.games.find((game) => [game.home, game.away].some((view) => view?.ending === 'desync'));
  if (desync) return `Game ${desync.number} went out of sync`;
  const lost = detail.games.find((game) => [game.home, game.away].some((view) => view?.ending === 'disconnect'));
  if (lost) return `Game ${lost.number} lost its connection`;
  const reporters = detail.sides.filter((side) => side.reporter);
  const cause = (name) => reporters.find((side) => side.reporter.cause === name);
  if (cause('desync')) return 'The series went out of sync';
  if (cause('disconnect')) return 'The connection was lost';
  if (cause('offline')) return `${cause('offline').mii}'s game went offline`;
  const left = reporters.find((side) => side.reporter.left_at);
  if (left) return `${left.mii}'s game ${LEFT_HOW[left.reporter.left_how] || 'left WFC'}`;
  return views.length ? null : 'It stopped before a game ended';
}

function sideReporter(side) {
  const reporter = side.reporter;
  if (!reporter) return h('span', { class: 'sub' }, 'No reports: a retail game, or its reports never came (see Monitoring)');
  return [
    h('a', { href: playerPath(reporter.profile_id) }, `Profile ${reporter.profile_id}`),
    h('span', { class: 'sub' }, ['Friend code ', h('code', null, reporter.friend_code)]),
    reporter.build && h('span', { class: 'sub' }, ['Build ', h('code', null, reporter.build)]),
  ];
}

function sideEnd(side, now) {
  const reporter = side.reporter;
  if (!reporter) return '—';
  const left = reporter.left_at && h('span', { class: 'sub' }, `Left WFC ${ago(reporter.left_at, now).textContent}: ${LEFT_HOW[reporter.left_how] || reporter.left_how}`);
  if (!reporter.reason) return [badge(null, 'No end yet'), h('span', { class: 'sub' }, ['last report ', ago(reporter.last_report_at, now)]), left];
  const [name, tone] = reporter.reason === 'decided' ? ['Decided', 'good'] : ['Abandoned', null];
  const cause = reporter.reason === 'abandoned' && CAUSES[reporter.cause];
  return [badge(tone, name), cause && h('span', { class: 'sub' }, cause[0]), h('span', { class: 'sub' }, ago(reporter.ended_at, now))];
}

/** What a desync's side knew: the first check that failed, and when. */
function syncText(sync) {
  const check = SYNC_CHECKS[sync.check] || sync.check;
  return sync.frame == null ? check : `${check} at frame ${integer.format(sync.frame)}`;
}

/** A game's ending, with what a desync's sides knew of it. */
function endingCell(row) {
  const views = row.other ? [row.game.home, row.game.away] : [row.view];
  const syncs = [...new Set(views.filter((view) => view.sync).map((view) => syncText(view.sync)))];
  // gameCell may give both sides' values: h() flattens one level only.
  return [gameCell(row, 'ending', (ending) => ENDINGS[ending] || ending), ...syncs.map((text) => h('span', { class: 'sub' }, text))].flat();
}

/** A value both sides reported: once when they agree, else each side's. */
function eachSide(home, away, format) {
  const [mine, theirs] = [home, away].map((value) => (value == null ? null : format(value)));
  if (mine == null || theirs == null || mine === theirs) return mine ?? theirs ?? '—';
  return [`${mine} (home)`, h('span', { class: 'sub' }, `${theirs} (away)`)];
}

function gameRows(games) {
  const rows = [];
  for (const game of games) {
    if (game.status === 'disputed') {
      rows.push({ game, seen: 'Home saw', view: game.home, first: true });
      rows.push({ game, seen: 'Away saw', view: game.away, first: false });
    } else {
      rows.push({ game, seen: game.status === 'agreed' ? 'Both' : game.status === 'only_home' ? 'Home' : 'Away', view: game.home || game.away, other: game.home && game.away, first: true });
    }
  }
  return rows;
}

/** A game's field: its view's, or both views' when both sides saw it the
 * same game (an unknown name gives way to a known one). */
function gameCell(row, field, format) {
  if (!row.other) return format(row.view[field]);
  const known = (value) => (value === 'unknown' ? null : value);
  return eachSide(known(row.game.home[field]), known(row.game.away[field]), format);
}

function renderMatch(data, now) {
  const detail = data.content;
  if (!detail) {
    if (data.error) return [unreachable(data.error)];
    return [banner({ level: 'warning', title: 'No such match', detail: `No game reported a match ${route.id}.` })];
  }
  const { match, sides, games, events } = detail;
  setHead(`${match.home.mii} vs ${match.away.mii}`, [
    `${MODES[match.mode] || match.mode} · ${match.release} · best of ${match.best_of} · started `,
    ago(match.started_at, now),
  ]);
  const bySide = { home: sides[0], away: sides[1] };
  const gameOf = (event) => games.find((game) => game.number === event.game);

  return [
    matchBanner(match, now, match.state === 'abandoned' ? stopCause(detail) : null),
    card(
      'Players',
      'Each side as its own game reported it',
      table(
        [
          { label: 'Side', cell: (side) => (side.side === 'home' ? 'Home' : 'Away') },
          { label: 'Mii', cell: (side) => (match.winner === side.side ? h('strong', null, side.mii) : side.mii) },
          { label: 'Reported by', cell: (side) => sideReporter(side) },
          { label: 'Captain', cell: (side) => captainName(side.captain) },
          { label: 'Sidekicks', cell: (side) => (side.sidekicks.length ? side.sidekicks.map((name) => SIDEKICK_NAMES[name] || name).join(', ') : h('span', { class: 'sub' }, 'unknown')) },
          { label: 'Games won', num: true, cell: (side) => `${match.score[side.side]}` },
          { label: 'End', cell: (side) => sideEnd(side, now) },
        ],
        sides,
        'No sides.',
      ),
    ),
    card(
      'Games',
      "A game counts when both games saw it the same, or only one reported it; each side's view where they differ",
      table(
        [
          { label: 'Game', cell: (row) => (row.first ? `${row.game.number}` : '') },
          {
            label: 'Status',
            cell: (row) => {
              if (!row.first) return '';
              const [name, tone] = GAME_STATUSES[row.game.status] || [row.game.status, null];
              return badge(tone, name);
            },
          },
          { label: 'Seen by', cell: (row) => row.seen },
          { label: 'Score', num: true, cell: (row) => `${row.view.score.home} – ${row.view.score.away}` },
          { label: 'Captains', cell: (row) => gameCell(row, 'captains', (c) => `${captainName(c.home)} vs ${captainName(c.away)}`) },
          { label: 'Stadium', cell: (row) => gameCell(row, 'stadium', (name) => STADIUM_NAMES[name] || name) },
          { label: 'Ending', cell: endingCell },
          { label: 'Length', cell: (row) => gameCell(row, 'seconds', gameLength) },
        ],
        gameRows(games),
        'No game reported yet.',
      ),
    ),
    card(
      'Reports',
      'As the games sent them, the first first; a report that came again is kept once',
      table(
        [
          { label: 'When', cell: (e) => [h('time', { datetime: e.at, title: dateTime.format(new Date(e.at)) }, clock.format(new Date(e.at))), h('span', { class: 'sub' }, ago(e.at, now))] },
          { label: 'Side', cell: (e) => [e.side === 'home' ? 'Home' : 'Away', h('span', { class: 'sub' }, bySide[e.side].mii)] },
          {
            label: 'Report',
            cell: (e) => {
              if (e.event === 'start') return 'Start';
              if (e.event === 'end') return `End: ${bySide[e.side].reporter?.reason || 'unknown'}`;
              const view = gameOf(e)?.[e.side];
              return view ? `Game ${e.game}: ${view.score.home} – ${view.score.away}` : `Game ${e.game}`;
            },
          },
          { label: 'Body', class: 'query', cell: (e) => h('code', { title: e.body }, e.body) },
        ],
        events,
        'No reports.',
      ),
    ),
  ];
}

// --- Monitoring ----------------------------------------------------------

/** What a DWC error code means; null for other codes. */
function errorMeaning(code) {
  if (code < 60000 || code > 99999) return null;
  const parts = [DWC_SEQUENCES[Math.floor(code / 10000)], DWC_PARTS[Math.floor((code % 10000) / 1000)], DWC_KINDS[code % 1000]];
  return parts.filter(Boolean).join(' · ') || null;
}

/** `part` of `whole` as a share, for a sub line. */
function share(part, whole) {
  return whole ? percent.format(part / whole) : '—';
}

/** A count that is not 0 stands out. */
function flagged(value, tone) {
  return value ? h('strong', { 'data-tone': tone, class: 'flag' }, integer.format(value)) : h('span', { class: 'zero' }, '0');
}

function incidentWhat(incident) {
  const [name, tone] = INCIDENTS[incident.kind] || [incident.kind, null];
  let detail = null;
  if (incident.kind === 'desync') {
    detail = incident.sync_check ? `Game ${incident.game} · ${syncText({ check: incident.sync_check, frame: incident.sync_frame })}` : `Game ${incident.game}`;
  } else if (incident.kind === 'disconnect') {
    detail = `Game ${incident.game}`;
  } else if (incident.kind === 'left') {
    detail = LEFT_HOW[incident.how] || incident.how;
  }
  return [badge(tone, name), detail && h('span', { class: 'sub' }, detail)];
}

// --- Monitoring timeline -------------------------------------------------

// What went wrong, in the timeline's order: the stack from its baseline up,
// each kind with its series color (admin.css, --series-2 to --series-5).
const PROBLEM_KINDS = [
  { kind: 'desync', name: 'Out of sync' },
  { kind: 'disconnect', name: 'Lost connection' },
  { kind: 'left', name: 'Left WFC' },
  { kind: 'error', name: 'Error codes' },
];
// The plots' heights in pixels, as admin.css sets them.
const MATCHES_HEIGHT = 80;
const PROBLEMS_HEIGHT = 120;
// Hours between the time axis' labels.
const AXIS_HOURS = 6;

/** 0, then whole steps of 1, 2 or 5 × 10^n up to at least `max`. */
function niceTicks(max) {
  const rough = Math.max(1, max) / 2;
  const power = 10 ** Math.floor(Math.log10(rough));
  const step = Math.max(1, [1, 2, 5, 10].map((m) => m * power).find((value) => value >= rough));
  const ticks = [0];
  while (ticks[ticks.length - 1] < max || ticks.length < 2) ticks.push(ticks[ticks.length - 1] + step);
  return ticks;
}

/** The timeline's columns from the server's rows, and the matches in progress
 * as a step function: `levels` holds [time, matches from then on]. */
function timelineModel(timeline, now) {
  const start = Date.parse(timeline.from);
  const step = timeline.column_minutes * 60000;
  const columns = [];
  const byStart = new Map();
  for (let at = Math.floor(start / step) * step; at < now; at += step) {
    const column = { at, end: at + step, kinds: {}, codes: new Map(), total: 0, peak: 0 };
    for (const { kind } of PROBLEM_KINDS) column.kinds[kind] = 0;
    columns.push(column);
    byStart.set(at, column);
  }
  const totals = Object.fromEntries(PROBLEM_KINDS.map(({ kind }) => [kind, 0]));
  for (const problem of timeline.problems) {
    const column = byStart.get(Date.parse(problem.at));
    if (!column || !(problem.kind in column.kinds)) continue;
    column.kinds[problem.kind] += problem.count;
    column.total += problem.count;
    totals[problem.kind] += problem.count;
    if (problem.code != null) column.codes.set(problem.code, (column.codes.get(problem.code) || 0) + problem.count);
  }

  // +1 where a match starts, -1 where it stops; ends first at a tie.
  let level = 0;
  const changes = [];
  for (const match of timeline.matches) {
    const begun = Date.parse(match.started_at);
    const stopped = match.ended_at ? Date.parse(match.ended_at) : now;
    if (stopped <= start || begun >= now || stopped <= begun) continue;
    if (begun <= start) level += 1;
    else changes.push([begun, 1]);
    if (stopped < now) changes.push([stopped, -1]);
  }
  changes.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  const levels = [[start, level]];
  for (const [at, delta] of changes) {
    level += delta;
    levels.push([at, level]);
  }
  // Each column's peak: its level at its start, then every change inside it.
  let index = 0;
  for (const column of columns) {
    while (index + 1 < levels.length && levels[index + 1][0] <= column.at) index += 1;
    let peak = levels[index][1];
    for (let next = index + 1; next < levels.length && levels[next][0] < column.end; next += 1) peak = Math.max(peak, levels[next][1]);
    column.peak = peak;
  }
  return {
    start,
    now,
    step,
    columns,
    totals,
    levels,
    current: level,
    most: Math.max(0, ...levels.map(([, value]) => value)),
    worst: Math.max(0, ...columns.map((column) => column.total)),
  };
}

/** A plot's horizontal hairlines and the y-axis labels for `ticks`. */
function yAxis(ticks, height) {
  const top = ticks[ticks.length - 1];
  const y = (value) => height - (value / top) * height;
  const labels = h('div', { class: 'tl-y', 'aria-hidden': 'true' });
  const grid = h('div', { class: 'tl-grid', 'aria-hidden': 'true' });
  for (const value of ticks) {
    const label = h('span', null, count(value));
    label.style.top = `${y(value)}px`;
    labels.append(label);
    const line = h('span', { class: value === 0 ? 'tl-base' : null });
    line.style.top = `${Math.min(height - 1, y(value))}px`;
    grid.append(line);
  }
  return { labels, grid, y };
}

function timelineChart(timeline, now) {
  const model = timelineModel(timeline, now);
  const { start, step, columns, totals } = model;
  const span = now - start;
  const x = (at) => ((at - start) / span) * 100;
  const hours = Math.round(span / 3600000);

  // Matches in progress: a step area, its line 2px whatever the width.
  const matchTicks = niceTicks(model.most);
  const matchAxis = yAxis(matchTicks, MATCHES_HEIGHT);
  const vx = (at) => ((at - start) / span) * 1000;
  // 1px in from the plot's edges, so a 2px line at 0 or the top shows whole.
  const vy = (value) => Math.min(MATCHES_HEIGHT - 1, Math.max(1, matchAxis.y(value))).toFixed(1);
  let path = `M0,${vy(model.levels[0][1])}`;
  for (const [at, value] of model.levels.slice(1)) path += ` H${vx(at).toFixed(2)} V${vy(value)}`;
  path += ' H1000';
  const area = svg('svg', { viewBox: `0 0 1000 ${MATCHES_HEIGHT}`, preserveAspectRatio: 'none', 'aria-hidden': 'true' });
  area.append(
    svg('path', { class: 'tl-area', d: `${path} V${MATCHES_HEIGHT} H0 Z` }),
    svg('path', { class: 'tl-line', d: path, 'vector-effect': 'non-scaling-stroke' }),
  );
  const endDot = h('span', { class: 'tl-dot' });
  endDot.style.top = `${matchAxis.y(model.current)}px`;
  const matchCross = h('span', { class: 'tl-cross' });
  const matchPlot = h('div', { class: 'tl-plot', style: { height: `${MATCHES_HEIGHT}px` } }, matchAxis.grid, h('div', { class: 'tl-clip' }, area), endDot, matchCross);

  // What went wrong: one stacked column per period, 2px apart.
  const problemTicks = niceTicks(model.worst);
  const problemAxis = yAxis(problemTicks, PROBLEMS_HEIGHT);
  const unit = PROBLEMS_HEIGHT / problemTicks[problemTicks.length - 1];
  const bars = h('div', { class: 'tl-clip' });
  const columnNodes = new Map();
  for (const column of columns) {
    if (!column.total) continue;
    const stack = h('div', { class: 'tl-stack' });
    let shown = 0;
    for (const { kind } of PROBLEM_KINDS) {
      const value = column.kinds[kind];
      if (!value) continue;
      // The 2px gaps come out of the segments above the first, so the stack
      // stays as tall as its total.
      const height = Math.max(2, value * unit - (shown ? 2 : 0));
      stack.append(h('span', { class: 'tl-seg', 'data-kind': kind, style: { height: `${height.toFixed(1)}px` } }));
      shown += 1;
    }
    const node = h('div', { class: 'tl-col' }, stack);
    node.style.left = `${x(column.at)}%`;
    node.style.width = `${(step / span) * 100}%`;
    bars.append(node);
    columnNodes.set(column.at, node);
  }
  const problemCross = h('span', { class: 'tl-cross' });
  const problemPlot = h(
    'div',
    { class: 'tl-plot', style: { height: `${PROBLEMS_HEIGHT}px` } },
    problemAxis.grid,
    bars,
    !model.worst && h('div', { class: 'tl-none' }, h('span', null, `Nothing went wrong in the last ${hours} hours.`)),
    problemCross,
  );

  // Local time: a label every AXIS_HOURS hours, the day's name at midnight.
  const axis = h('div', { class: 'tl-x', 'aria-hidden': 'true' });
  const tick = new Date(start);
  tick.setMinutes(0, 0, 0);
  while (tick.getTime() <= start || tick.getHours() % AXIS_HOURS) tick.setHours(tick.getHours() + 1);
  for (; tick.getTime() < now; tick.setHours(tick.getHours() + AXIS_HOURS)) {
    const left = x(tick.getTime());
    if (left > 94) break;
    const label = h('span', null, tick.getHours() === 0 ? weekdayDay.format(tick) : hourMinute.format(tick));
    label.style.left = `${left}%`;
    axis.append(label);
    for (const plot of [matchPlot, problemPlot]) {
      const line = h('span', { class: 'tl-vline', 'aria-hidden': 'true' });
      line.style.left = `${left}%`;
      plot.prepend(line);
    }
  }
  axis.append(h('span', { class: 'tl-now' }, 'now'));

  // The readout for the picked column: every series, values first.
  const tip = h('div', { class: 'tl-tip', 'aria-hidden': 'true' });
  const chart = h(
    'div',
    {
      class: 'tl-chart',
      tabindex: '0',
      role: 'group',
      'aria-label': `Last ${hours} hours: ${model.current} matches in progress now, at most ${model.most} at once; ${count(columns.reduce((sum, column) => sum + column.total, 0))} problems. Arrow keys step through the half hours; the table below lists them.`,
    },
    h('div', { class: 'tl-head' }, h('span', { class: 'tl-key', 'data-kind': 'matches' }), h('strong', null, 'Matches in progress'), h('span', { class: 'sub' }, `${count(model.current)} now · ${count(model.most)} at most`)),
    h('div', { class: 'tl-row' }, matchAxis.labels, matchPlot),
    h(
      'div',
      { class: 'tl-head' },
      h('strong', null, 'What went wrong'),
      PROBLEM_KINDS.map(({ kind, name }) => h('span', { class: 'tl-legend' }, h('span', { class: 'tl-swatch', 'data-kind': kind }), name, h('span', { class: 'sub' }, count(totals[kind])))),
    ),
    h('div', { class: 'tl-row' }, problemAxis.labels, problemPlot),
    h('div', { class: 'tl-row' }, h('span'), axis),
    tip,
  );

  const show = () => {
    for (const node of columnNodes.values()) node.classList.remove('is-active');
    const column = columns.find((candidate) => candidate.at === timelineView.column);
    if (!column) {
      timelineView.column = null;
      chart.classList.remove('is-picking');
      return;
    }
    chart.classList.add('is-picking');
    columnNodes.get(column.at)?.classList.add('is-active');
    const middle = Math.min(100, Math.max(0, x(column.at + step / 2)));
    for (const cross of [matchCross, problemCross]) cross.style.left = `${middle}%`;
    const value = (number) => h('strong', { class: number ? null : 'zero' }, count(number));
    const codes = [...column.codes].sort((a, b) => b[1] - a[1] || a[0] - b[0]);
    tip.replaceChildren(
      ...[
        h('span', { class: 'tl-tip-when' }, `${weekdayDay.format(column.at)} ${hourMinute.format(column.at)}–${hourMinute.format(column.end)}`),
        h('span', { class: 'tl-tip-row' }, h('span', { class: 'tl-key', 'data-kind': 'matches' }), value(column.peak), 'matches at most'),
        ...PROBLEM_KINDS.map(({ kind, name }) => h('span', { class: 'tl-tip-row' }, h('span', { class: 'tl-key', 'data-kind': kind }), value(column.kinds[kind]), name)),
        codes.length > 0 && h('span', { class: 'tl-tip-codes' }, codes.map(([code, times]) => (times > 1 ? `${code} ×${times}` : `${code}`)).join(' · ')),
      ].filter(Boolean),
    );
    // Beside the crosshair, on the side with room.
    tip.style.left = `calc(var(--tl-gutter) + (100% - var(--tl-gutter)) * ${middle / 100})`;
    tip.dataset.side = middle > 60 ? 'left' : 'right';
  };
  const pick = (event) => {
    const rect = problemPlot.getBoundingClientRect();
    const fraction = (event.clientX - rect.left) / rect.width;
    timelineView.column = fraction < 0 || fraction > 1 ? null : Math.floor((start + fraction * span) / step) * step;
    show();
  };
  chart.addEventListener('pointermove', pick);
  chart.addEventListener('pointerdown', pick);
  chart.addEventListener('pointerleave', () => {
    if (timelineView.focused) return;
    timelineView.column = null;
    show();
  });
  chart.addEventListener('keydown', (event) => {
    const index = columns.findIndex((column) => column.at === timelineView.column);
    const last = columns.length - 1;
    const moves = { ArrowLeft: Math.max(0, index - 1), ArrowRight: index < 0 ? last : Math.min(last, index + 1), Home: 0, End: last };
    if (event.key === 'Escape') timelineView.column = null;
    else if (event.key in moves) timelineView.column = columns[moves[event.key]].at;
    else return;
    event.preventDefault();
    show();
  });
  chart.addEventListener('focus', () => {
    timelineView.focused = true;
    if (timelineView.column == null) timelineView.column = columns[columns.length - 1].at;
    show();
  });
  // A poll replaces the chart: only a blur that leaves it in the page is
  // the reader moving on.
  chart.addEventListener('blur', () => {
    setTimeout(() => {
      if (!chart.isConnected || document.activeElement === chart) return;
      timelineView.focused = false;
      timelineView.column = null;
      show();
    });
  });
  // Back to the new chart once render() has put it in the page.
  if (timelineView.focused) queueMicrotask(() => chart.isConnected && chart.focus({ preventScroll: true }));
  show();

  // The same numbers without the chart: every half hour with something in it.
  const rows = columns.filter((column) => column.total || column.peak).reverse();
  const details = h(
    'details',
    { class: 'tl-table', open: timelineView.table },
    h('summary', null, 'Show as a table'),
    table(
      [
        { label: 'From', cell: (c) => `${weekdayDay.format(c.at)} ${hourMinute.format(c.at)}` },
        { label: 'Matches at most', num: true, cell: (c) => integer.format(c.peak) },
        ...PROBLEM_KINDS.map(({ kind, name }) => ({ label: name, num: true, cell: (c) => flagged(c.kinds[kind], kind === 'desync' ? 'serious' : 'warning') })),
        { label: 'Codes', class: 'wrap', cell: (c) => [...c.codes].map(([code, times]) => (times > 1 ? `${code} ×${times}` : `${code}`)).join(', ') },
      ],
      rows,
      `No matches and nothing wrong in the last ${hours} hours.`,
    ),
  );
  details.addEventListener('toggle', () => {
    timelineView.table = details.open;
  });

  return h(
    'section',
    { class: 'card' },
    h('header', { class: 'card-head' }, h('h2', null, `Last ${hours} hours`), h('p', null, `Matches Recharged builds reported, and what went wrong, by ${timeline.column_minutes} minutes; your local time`)),
    h('div', { class: 'timeline' }, chart, details),
  );
}

function renderMonitoring(data, now) {
  const content = data.content;
  if (!content) return [unreachable(data.error)];
  const { days, by_day: byDay, by_build: byBuild, by_stadium: byStadium, incidents, errors, refusals } = content;
  const sum = (field) => byDay.reduce((total, day) => total + day[field], 0);
  const [matches, games] = [sum('matches'), sum('games')];
  const recent = refusals.filter((refusal) => since(refusal.last_at, now) < days * 86400);
  const window = `Last ${days} days`;
  const build = (name) => (name ? h('code', null, name) : h('span', { class: 'sub' }, 'Not named'));

  return [
    timelineChart(content.timeline, now),
    h(
      'section',
      { class: 'tiles', 'aria-label': 'Key figures' },
      tile('Games', count(games), `${count(matches)} matches · ${window.toLowerCase()}`),
      tile('Out of sync', count(sum('desyncs')), `${share(sum('desyncs'), games)} of games`),
      tile('Lost connection', count(sum('disconnects')), `${share(sum('disconnects'), games)} of games`),
      tile('Abandoned', count(sum('abandoned')), `${share(sum('abandoned'), matches)} of matches`),
      tile('Left WFC mid-match', count(sum('left')), 'Games that left before their end'),
      tile('Error codes', count(sum('errors')), `${count(errors.length)} different`),
      tile('Refused reports', count(recent.length), `${recent.length === 1 ? 'Profile' : 'Profiles'} refused lately`),
    ),
    card(
      'Incidents',
      `${window}, the newest first: what went wrong for a side of a match`,
      table(
        [
          { label: 'When', cell: (i) => [h('time', { datetime: i.at, title: dateTime.format(new Date(i.at)) }, ago(i.at, now))] },
          { label: 'What', cell: incidentWhat },
          { label: 'Player', cell: (i) => [i.mii, h('span', { class: 'sub' }, h('a', { href: playerPath(i.profile_id) }, `Profile ${i.profile_id}`))] },
          { label: 'Match', cell: (i) => [h('a', { href: `/admin/matches/${i.match_id}` }, `Match ${i.match_id}`), h('span', { class: 'sub' }, i.side === 'home' ? 'Home' : 'Away')] },
          { label: 'Build', cell: (i) => build(i.build) },
        ],
        incidents,
        `Nothing went wrong in the last ${days} days.`,
      ),
    ),
    card(
      'Error codes',
      `${window}: the numbers error popups showed players, the most frequent first`,
      table(
        [
          { label: 'Code', cell: (e) => [h('code', null, String(e.code)), errorMeaning(e.code) && h('span', { class: 'sub' }, errorMeaning(e.code))] },
          { label: 'Times', num: true, cell: (e) => integer.format(e.count) },
          { label: 'Players', num: true, cell: (e) => [integer.format(e.players), h('span', { class: 'sub' }, `${integer.format(e.addresses)} ${e.addresses === 1 ? 'address' : 'addresses'}`)] },
          { label: 'When', class: 'wrap', cell: (e) => (e.phases.length ? e.phases.map((phase) => PHASES[phase] || phase).join(', ') : h('span', { class: 'sub' }, 'unknown')) },
          { label: 'Builds', class: 'wrap', cell: (e) => (e.builds.length ? e.builds.flatMap((name, index) => [index ? ', ' : null, h('code', null, name)]) : h('span', { class: 'sub' }, 'not named')) },
          { label: 'Last', cell: (e) => ago(e.last_at, now) },
        ],
        errors,
        `No error codes in the last ${days} days.`,
      ),
    ),
    card(
      'By day',
      'UTC days; a game counts once, however many sides reported it',
      table(
        [
          { label: 'Day', cell: (d) => calendarDay.format(new Date(d.day)) },
          { label: 'Matches', num: true, cell: (d) => integer.format(d.matches) },
          { label: 'Abandoned', num: true, cell: (d) => flagged(d.abandoned, 'warning') },
          { label: 'Games', num: true, cell: (d) => integer.format(d.games) },
          { label: 'Out of sync', num: true, cell: (d) => flagged(d.desyncs, 'serious') },
          { label: 'Lost connection', num: true, cell: (d) => flagged(d.disconnects, 'warning') },
          { label: 'Left WFC', num: true, cell: (d) => flagged(d.left, 'warning') },
          { label: 'Errors', num: true, cell: (d) => flagged(d.errors, 'warning') },
        ],
        byDay,
        'No days.',
      ),
    ),
    card(
      'By build',
      "Each build's own series and the games it reported; a build from before reports named it shows as not named",
      table(
        [
          { label: 'Build', cell: (b) => build(b.build) },
          { label: 'Series', num: true, cell: (b) => integer.format(b.sides) },
          { label: 'Games', num: true, cell: (b) => integer.format(b.games) },
          { label: 'Out of sync', num: true, cell: (b) => [flagged(b.desyncs, 'serious'), h('span', { class: 'sub' }, share(b.desyncs, b.games))] },
          { label: 'Lost connection', num: true, cell: (b) => [flagged(b.disconnects, 'warning'), h('span', { class: 'sub' }, share(b.disconnects, b.games))] },
          { label: 'Left WFC', num: true, cell: (b) => flagged(b.left, 'warning') },
          { label: 'Last seen', cell: (b) => ago(b.last_seen, now) },
        ],
        byBuild,
        `No series reported in the last ${days} days.`,
      ),
    ),
    card(
      'By stadium',
      'Where games go out of sync',
      table(
        [
          { label: 'Stadium', cell: (s) => STADIUM_NAMES[s.stadium] || s.stadium },
          { label: 'Games', num: true, cell: (s) => integer.format(s.games) },
          { label: 'Out of sync', num: true, cell: (s) => [flagged(s.desyncs, 'serious'), h('span', { class: 'sub' }, share(s.desyncs, s.games))] },
          { label: 'Lost connection', num: true, cell: (s) => [flagged(s.disconnects, 'warning'), h('span', { class: 'sub' }, share(s.disconnects, s.games))] },
        ],
        byStadium,
        `No games reported in the last ${days} days.`,
      ),
    ),
    card(
      'Refused reports',
      'Reports of known profiles the server did not take, the latest first: a game whose matches show no reports from its side is often here',
      table(
        [
          { label: 'Player', cell: (r) => [r.mii || 'Unknown', h('span', { class: 'sub' }, h('a', { href: playerPath(r.profile_id) }, `Profile ${r.profile_id}`))] },
          { label: 'Why', cell: (r) => [REFUSALS[r.kind] || r.kind, h('span', { class: 'sub' }, r.problem)] },
          { label: 'Reports', num: true, cell: (r) => integer.format(r.count) },
          { label: 'From', cell: (r) => h('code', null, r.address) },
          { label: 'Last', cell: (r) => [ago(r.last_at, now), h('span', { class: 'sub' }, `first ${ago(r.first_at, now).textContent}`)] },
        ],
        refusals,
        'No report was refused.',
      ),
    ),
  ];
}

// --- Leaderboards --------------------------------------------------------

function renderLeaderboards(data, now) {
  const content = data.content;
  if (!content) return [unreachable(data.error)];
  const { totals, categories, top } = content;

  const choices = h(
    'div',
    { class: 'segmented', role: 'group', 'aria-label': 'Board' },
    CATEGORIES.map((name, index) =>
      h(
        'button',
        {
          type: 'button',
          'aria-pressed': String(index === category),
          onclick: () => {
            category = index;
            render(lastData);
          },
        },
        name,
      ),
    ),
  );
  const leaders = top.filter((entry) => entry.category === category);
  const past = (content.past || []).find((board) => board.category === category);
  const standings = [
    { label: 'Rank', num: true, cell: (e) => `${e.rank}` },
    { label: 'Player', cell: (e) => [e.name || 'Unknown', h('span', { class: 'sub' }, `Profile ${e.profile_id}`)] },
    { label: 'Region', cell: (e) => REGIONS[e.region] || `${e.region}` },
    { label: 'Score', num: true, cell: (e) => integer.format(e.score) },
    { label: 'Wins', num: true, cell: (e) => (e.wins == null ? '—' : integer.format(e.wins)) },
    { label: 'Losses', num: true, cell: (e) => (e.losses == null ? '—' : integer.format(e.losses)) },
  ];
  const daily = DAILY_BOARDS.has(category);
  const pastNote = past
    ? `${calendarDay.formatRange(new Date(past.first_day), new Date(past.last_day))} · ${count(past.players)} ${past.players === 1 ? 'player' : 'players'} · kept when the game started the next one`
    : `The final standings, once a ${daily ? 'day' : 'season'} with scores has ended`;

  return [
    h(
      'section',
      { class: 'tiles', 'aria-label': 'Key figures' },
      tile('Entries', count(totals.entries), 'Rows in scores'),
      tile('Players', count(totals.players), 'Distinct profile IDs'),
      tile('Last score', totals.last_put ? ago(totals.last_put, now) : '—', totals.last_put ? dateTime.format(new Date(totals.last_put)) : 'No scores yet'),
      tile('Boards in use', [`${categories.length}`, h('small', null, `/ ${CATEGORIES.length}`)], 'Categories with scores'),
    ),
    card(
      'Leaders',
      'The first 10 of a board, ranked as the game ranks them',
      table(
        [...standings, { label: 'Updated', cell: (e) => ago(e.updated_at, now) }],
        leaders,
        `No scores on ${CATEGORIES[category]} yet.`,
      ),
      choices,
    ),
    card(
      daily ? 'Last day' : 'Last season',
      pastNote,
      table(
        [...standings, { label: 'Last played', cell: (e) => dateTime.format(new Date(e.updated_at)) }],
        past ? past.top : [],
        `No ${daily ? 'day' : 'season'} of ${CATEGORIES[category]} has ended with scores yet.`,
      ),
    ),
    card(
      'Boards',
      ['Entries by category and region, from ', h('code', null, 'scores')],
      table(
        [
          { label: 'Category', cell: (c) => [CATEGORIES[c.category] || `Category ${c.category}`, h('span', { class: 'sub' }, `#${c.category}`)] },
          { label: 'Entries', num: true, cell: (c) => integer.format(c.players) },
          { label: 'JP', num: true, cell: (c) => integer.format(c.jp) },
          { label: 'US', num: true, cell: (c) => integer.format(c.us) },
          { label: 'EU', num: true, cell: (c) => integer.format(c.eu) },
          { label: 'KR', num: true, cell: (c) => integer.format(c.kr) },
          { label: 'Top score', num: true, cell: (c) => integer.format(c.top_score) },
          { label: 'Last score', cell: (c) => ago(c.last_put, now) },
        ],
        categories,
        'No scores yet. They appear after the first match played online.',
      ),
    ),
  ];
}

// --- Database ------------------------------------------------------------

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

/** Turns the transaction counters into a rate since the last poll. */
function sample(data) {
  const db = data.content;
  if (!db) return;
  const at = new Date(data.now);
  const total = db.commits + db.rollbacks;
  if (previous && total >= previous.total && at > previous.at) {
    samples.push({ at, rate: (total - previous.total) / ((at - previous.at) / 1000) });
    samples = samples.slice(-HISTORY);
  }
  previous = { at, total };
}

function renderDatabase(data, now) {
  const db = data.content;
  const pool = data.server.pool;
  if (!db) {
    return [
      unreachable(data.error),
      h(
        'section',
        { class: 'tiles' },
        tile('Server uptime', duration(since(data.server.started_at, now)), `Revision ${data.server.revision.slice(0, 7)}`),
        tile('Connection pool', `${pool.size}`, `${pool.idle} idle of ${pool.max} max`),
      ),
    ];
  }

  const usage = db.open_connections / db.max_connections;
  const meter = h(
    'div',
    {
      class: 'meter',
      'data-level': usage >= 0.9 ? 'serious' : usage >= 0.75 ? 'warning' : null,
      role: 'meter',
      'aria-valuemin': 0,
      'aria-valuemax': db.max_connections,
      'aria-valuenow': db.open_connections,
      'aria-label': 'Connections used',
    },
    h('div', { class: 'meter-fill' }),
  );
  meter.firstChild.style.width = `${Math.min(100, usage * 100)}%`;
  const blocks = db.blocks_hit + db.blocks_read;
  const rows = db.tables.reduce((sum, table) => sum + table.rows, 0);
  const rate = samples.at(-1);
  const tone = { active: 'accent', 'idle in transaction': 'warning', 'idle in transaction (aborted)': 'critical' };

  return [
    banner(databaseHealth(db, now)),
    h(
      'section',
      { class: 'tiles', 'aria-label': 'Key figures' },
      tile('Uptime', duration(since(db.started_at, now)), `Since ${dateTime.format(new Date(db.started_at))}`),
      tile('Size', bytes(db.size_bytes), `${db.tables.length} tables · ≈${count(rows)} rows`),
      tile('Connections', [`${db.open_connections}`, h('small', null, `/ ${db.max_connections}`)], `${db.active_connections} active · pool ${pool.size}/${pool.max}`, meter),
      tile('Cache hit ratio', blocks ? percent.format(db.blocks_hit / blocks) : '—', `${count(db.blocks_read)} blocks read from disk`),
      tile('Transactions', rate ? [rate.rate.toFixed(1), h('small', null, '/s')] : '—', `${count(db.commits)} commits · ${count(db.rollbacks)} rollbacks`, sparkline()),
    ),
    card(
      'Connections',
      'Clients of PostgreSQL, besides this dashboard',
      table(
        [
          { label: 'PID', num: true, cell: (c) => `${c.pid}` },
          { label: 'State', cell: (c) => badge(tone[c.state], c.state || 'unknown') },
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
    ),
    card(
      'Tables',
      "Row counts are PostgreSQL's estimates",
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
    ),
    card(
      'Migrations',
      'Applied by the server when it starts',
      table(
        [
          { label: 'Version', cell: (m) => h('code', null, `${m.version}`) },
          { label: 'Description', cell: (m) => m.description },
          { label: 'Applied', cell: (m) => ago(m.installed_on, now) },
          { label: 'Took', num: true, cell: (m) => (m.execution_ms < 1 ? '<1 ms' : `${integer.format(m.execution_ms)} ms`) },
          { label: 'Status', cell: (m) => badge(m.success ? 'good' : 'critical', m.success ? 'Applied' : 'Failed') },
        ],
        db.migrations,
        'No migrations applied.',
      ),
    ),
  ];
}

// --- Logs ----------------------------------------------------------------

/** The Logs page's controls. They stay out of the polled page, so typing
 * survives each refresh. */
const logs = {
  node: null,
  day: null, // The file shown, or null for the newest.
  level: '', // The least severe level shown, or '' for all.
  text: '',
  paused: false,
  typing: null, // The search's pending refresh.

  query() {
    const params = new URLSearchParams();
    if (this.day) params.set('day', this.day);
    if (this.level) params.set('level', this.level);
    if (this.text) params.set('q', this.text);
    const query = params.toString();
    return query && `?${query}`;
  },

  load(content) {
    if (!this.node) this.build();
    const days = content.files.map((file) => file.day);
    // The file asked for is gone: follow the newest again.
    if (this.day && !days.includes(this.day)) this.day = null;
    const select = this.node.querySelector('select');
    if (select.dataset.days !== days.join()) {
      select.dataset.days = days.join();
      select.replaceChildren(
        ...days.map((day, index) => h('option', { value: day }, `${calendarDay.format(new Date(day))}${index ? '' : ' · newest'}`)),
      );
    }
    select.value = content.day;
  },

  build() {
    const select = h('select', {
      'aria-label': 'Day',
      onchange: () => {
        this.day = select.selectedIndex > 0 ? select.value : null;
        poll();
      },
    });
    const levels = h(
      'div',
      { class: 'segmented', role: 'group', 'aria-label': 'Levels' },
      [['', 'All'], ['warn', 'Warnings and errors'], ['error', 'Errors']].map(([level, name]) =>
        h(
          'button',
          {
            type: 'button',
            'aria-pressed': String(level === this.level),
            onclick: (event) => {
              this.level = level;
              for (const button of levels.children) button.setAttribute('aria-pressed', String(button === event.currentTarget));
              poll();
            },
          },
          name,
        ),
      ),
    );
    const search = h('input', {
      type: 'search',
      placeholder: 'Search',
      'aria-label': 'Search the lines',
      oninput: () => {
        clearTimeout(this.typing);
        this.typing = setTimeout(() => {
          this.text = search.value.trim();
          poll();
        }, 300);
      },
    });
    const pause = h(
      'button',
      {
        class: 'button',
        type: 'button',
        'aria-pressed': 'false',
        onclick: () => {
          this.paused = !this.paused;
          pause.setAttribute('aria-pressed', String(this.paused));
          pause.textContent = this.paused ? 'Resume' : 'Pause';
          if (!this.paused) poll();
          else {
            clearTimeout(timer);
            setLive('paused', 'Paused');
          }
        },
      },
      'Pause',
    );
    this.node = h('section', { class: 'card log-tools', 'aria-label': 'Filters' }, select, levels, search, pause);
    document.getElementById('body').before(this.node);
  },
};

function renderLogs(data, now) {
  const content = data.content;
  if (!content) return [banner({ level: 'critical', title: 'Logs unreadable', detail: data.error || 'No answer.' })];
  if (!content.dir) {
    return [banner({ level: 'warning', title: 'No log files', detail: 'The server logs to stderr only. Set LOG_DIR to keep a file per day and read it here.' })];
  }
  if (!content.files.length) {
    return [banner({ level: 'pending', title: 'No log files yet', detail: `The server writes them to ${content.dir}.` })];
  }
  logs.load(content);
  const filtered = logs.level || logs.text;
  const note = [
    `${integer.format(content.matched)} ${filtered ? 'matching ' : ''}${content.matched === 1 ? 'line' : 'lines'}, newest first`,
    content.lines.length < content.matched && `the last ${integer.format(content.lines.length)} shown`,
    content.partial && `only the file's last ${bytes(content.scanned_bytes)} searched: download it for the rest`,
  ].filter(Boolean).join(' · ');

  const lines = table(
    [
      {
        label: 'Time',
        cell: (line) => h('time', { datetime: line.time, title: dateTime.format(new Date(line.time)) }, clock.format(new Date(line.time))),
      },
      {
        label: 'Level',
        cell: (line) => {
          const [name, tone] = LEVELS[line.level] || [line.level, null];
          return badge(tone, name);
        },
      },
      { label: 'Source', cell: (line) => (line.target ? h('code', null, line.target) : '—') },
      { label: 'Message', class: 'log-text', cell: (line) => line.text },
    ],
    content.lines,
    filtered ? 'No lines match.' : 'Nothing logged on this day yet.',
  );
  lines.classList.add('log-lines');

  return [
    card(calendarDay.format(new Date(content.day)), note, lines),
    card(
      'Files',
      `One per day (UTC) in ${content.dir}; the oldest go after LOG_RETENTION_DAYS`,
      table(
        [
          { label: 'Day', cell: (file) => [calendarDay.format(new Date(file.day)), file.day === content.day && h('span', { class: 'sub' }, 'Shown above')] },
          { label: 'Size', num: true, cell: (file) => bytes(file.bytes) },
          { label: 'Last written', cell: (file) => ago(file.modified, now) },
          { label: '', class: 'actions', cell: (file) => h('a', { class: 'button', href: `/admin/api/logs/${file.day}`, download: '' }, 'Download') },
        ],
        content.files,
        'No files.',
      ),
    ),
  ];
}

// --- Competitive ---------------------------------------------------------

function gameLength(seconds) {
  const minutes = Math.floor(seconds / 60);
  return seconds % 60 ? `${minutes} min ${seconds % 60} s` : `${minutes} min`;
}

function rulesSummary(rules) {
  const end = rules.limit === 'time' ? `${gameLength(rules.time)} games` : `First to ${rules.goals} goals`;
  return [
    `${end}, best of ${rules.games}`,
    `${SKILL_NAMES[rules.skill] || rules.skill}`,
    STADIUM_NAMES[rules.stadium] || rules.stadium,
  ].join(' · ');
}

function renderCompetitive(data, now) {
  if (!data.content) return [unreachable(data.error)];
  const { rule_sets: ruleSets, totals, matches } = data.content;
  editor.load(data.content.choices);
  const sets = card(
    'Rule sets',
    'What a match is played by. A challenge picks one; a match keeps the rules it was made with.',
    table(
      [
        { label: 'Name', cell: (set) => h('strong', null, set.name) },
        { label: 'Tweaks', class: 'wrap', cell: (set) => (set.rules.tweaks.length ? set.rules.tweaks.map((tweak) => TWEAK_NAMES[tweak] || tweak).join(', ') : h('span', { class: 'sub' }, 'none')) },
        { label: 'Rules', class: 'wrap', cell: (set) => rulesSummary(set.rules) },
        { label: '', class: 'actions', cell: (set) => h('button', { class: 'button', type: 'button', onclick: () => editor.edit(set) }, 'Edit') },
      ],
      ruleSets,
      'No rule sets yet: add one below.',
    ),
  );
  const played = card(
    'Matches',
    'The latest sets between players who signed in.',
    table(
      [
        { label: 'Made', cell: (match) => ago(match.created_at, now) },
        { label: 'Challenger', cell: (match) => match.challenger },
        { label: 'Challenged', cell: (match) => match.challenged },
        { label: 'Rules', cell: (match) => match.rule_set },
        {
          label: 'State',
          cell: (match) => {
            const [text, tone] = MATCH_STATES[match.state] || [match.state, null];
            return badge(tone, text);
          },
        },
        { label: 'Winner', cell: (match) => match.winner || (match.state === 'finished' ? 'Draw' : h('span', { class: 'sub' }, '–')) },
      ],
      matches,
      'No matches yet.',
    ),
  );
  return [
    h(
      'div',
      { class: 'tiles' },
      tile('Players', count(totals.players), `${count(totals.discord)} with Discord · ${count(totals.test)} test`),
      tile('Matches', count(totals.matches), `${count(totals.finished)} finished`),
      tile('Rule sets', count(ruleSets.length), 'for challenges'),
    ),
    sets,
    played,
  ];
}

/** The rule set editor. It stays out of the polled page, so typing survives
 * each refresh. */
const editor = {
  node: null,
  choices: null,
  editing: null, // The rule set's id, or null for a new one.

  load(choices) {
    if (this.choices) return;
    this.choices = choices;
    this.render(null);
  },

  edit(set) {
    this.render(set);
    this.node.scrollIntoView({ behavior: 'smooth', block: 'start' });
  },

  field(label, control) {
    return h('label', { class: 'editor-field' }, h('span', null, label), control);
  },

  select(id, values, names, selected) {
    return h(
      'select',
      { id },
      values.map((value) => h('option', { value, selected: String(value) === String(selected) }, names ? names[value] || value : String(value))),
    );
  },

  render(set) {
    const rules = set
      ? set.rules
      : { tweaks: [], skill: 'rookie', limit: 'time', time: 180, goals: 5, games: 3, stadium: 'random' };
    this.editing = set ? set.id : null;
    const choices = this.choices;
    const body = h(
      'form',
      { class: 'editor', onsubmit: (event) => { event.preventDefault(); this.save(); } },
      this.field('Name', h('input', { id: 'rule-name', value: set ? set.name : '', maxlength: '48', required: true })),
      h(
        'fieldset',
        { class: 'editor-field' },
        h('legend', null, 'Gameplay tweaks for both teams'),
        choices.tweaks.map((tweak) =>
          h('label', { class: 'editor-check' }, h('input', { type: 'checkbox', name: 'tweak', value: tweak, checked: rules.tweaks.includes(tweak) }), TWEAK_NAMES[tweak] || tweak),
        ),
      ),
      h(
        'div',
        { class: 'editor-row' },
        this.field('Skill level', this.select('rule-skill', choices.skills, SKILL_NAMES, rules.skill)),
        this.field('A game ends', this.select('rule-limit', ['time', 'goals'], { time: 'On time', goals: 'On goals' }, rules.limit)),
        this.field('Seconds', h('input', { id: 'rule-time', type: 'number', min: '60', max: '900', step: '30', value: rules.time })),
        this.field('Goals', h('input', { id: 'rule-goals', type: 'number', min: '1', max: '10', value: rules.goals })),
      ),
      h(
        'div',
        { class: 'editor-row' },
        this.field('Games in the series', this.select('rule-games', choices.series, null, rules.games)),
        this.field('Stadium', this.select('rule-stadium', ['random', ...choices.stadiums], STADIUM_NAMES, rules.stadium)),
      ),
      h(
        'div',
        { class: 'editor-actions' },
        h('button', { class: 'button primary', type: 'submit' }, set ? 'Save changes' : 'Add rule set'),
        set && h('button', { class: 'button', type: 'button', onclick: () => this.render(null) }, 'New rule set'),
        set && h('button', { class: 'button danger', type: 'button', onclick: () => this.remove() }, 'Delete'),
        h('span', { class: 'editor-status', id: 'rule-status', role: 'status' }),
      ),
    );
    const node = h('section', { class: 'card' }, h('header', { class: 'card-head' }, h('h2', null, set ? `Edit ${set.name}` : 'New rule set'), h('p', null, 'Both games apply these rules; the server pairs two games only once both confirmed them.')), body);
    if (this.node) this.node.replaceWith(node);
    else document.getElementById('content').append(node);
    this.node = node;
  },

  status(text, tone) {
    const status = document.getElementById('rule-status');
    status.textContent = text;
    status.dataset.tone = tone || '';
  },

  async save() {
    const value = (id) => document.getElementById(id).value;
    const form = {
      name: value('rule-name'),
      rules: {
        tweaks: [...this.node.querySelectorAll('input[name=tweak]:checked')].map((box) => box.value),
        skill: value('rule-skill'),
        limit: value('rule-limit'),
        time: Number(value('rule-time')),
        goals: Number(value('rule-goals')),
        games: Number(value('rule-games')),
        stadium: value('rule-stadium'),
      },
    };
    try {
      const path = this.editing ? `/admin/api/rules/${this.editing}` : '/admin/api/rules';
      const saved = await send(this.editing ? 'PUT' : 'POST', path, form);
      this.render(saved);
      this.status('Saved', 'good');
      poll();
    } catch (error) {
      this.status(error.message, 'critical');
    }
  },

  async remove() {
    if (!window.confirm('Delete this rule set? Matches made with it keep their rules.')) return;
    try {
      await send('DELETE', `/admin/api/rules/${this.editing}`);
      this.render(null);
      this.status('Deleted', 'good');
      poll();
    } catch (error) {
      this.status(error.message, 'critical');
    }
  },
};

// --- Maintenance ---------------------------------------------------------

/** `1 entry`, `2 entries`. */
function plural(value, one, many) {
  return `${integer.format(value)} ${value === 1 ? one : many}`;
}

/** A warning inside a card or dialog. */
function warningNote(...text) {
  return h('p', { class: 'warning-note' }, icon('warning'), h('span', null, ...text));
}

/** The games online keep their totals (docs/gamestats2.md, "Seasons"). */
function onlineWarning(online) {
  if (!online) return null;
  return warningNote(
    `${count(online)} ${online === 1 ? 'game is' : 'games are'} online now. `,
    'A game keeps its totals until it logs in again, and its next match puts them back. Wipe when no one is online.',
  );
}

function renderMaintenance(data) {
  if (!data.content) return [unreachable(data.error)];
  const boards = data.content.leaderboards;
  const empty = boards.entries + boards.past_entries === 0;
  return [
    h(
      'section',
      { class: 'tiles', 'aria-label': 'What a wipe deletes' },
      tile('Entries', count(boards.entries), 'Every board, as it stands'),
      tile('Past seasons', count(boards.past_entries), 'Final standings kept'),
      tile('Players', count(boards.players), 'With a score in either'),
      tile('Online now', count(boards.online), 'Games that keep their totals'),
    ),
    h(
      'section',
      { class: 'card danger-zone' },
      h('header', { class: 'card-head' }, h('h2', null, 'Wipe all leaderboards'), h('p', null, 'Asks twice · cannot be undone')),
      h(
        'div',
        { class: 'operation' },
        h('p', null, 'Deletes every score on every board (season, striker of the day, friends and the alternate boards) and the final standings kept from past seasons. Profiles, matches and rule sets stay.'),
        h('p', null, 'Each game then finds no score of its own when it logs in, and starts again from 0.'),
        onlineWarning(boards.online),
        h(
          'div',
          { class: 'editor-actions' },
          h('button', { class: 'button destroy', type: 'button', disabled: empty || wipe.busy, onclick: () => wipe.ask() }, 'Wipe all leaderboards…'),
          wipe.result
            ? h('span', { class: 'editor-status', role: 'status', 'data-tone': wipe.result[1] }, wipe.result[0])
            : empty && h('span', { class: 'editor-status', role: 'status' }, 'Nothing to wipe: the boards are empty.'),
        ),
      ),
    ),
  ];
}

/** Wiping the leaderboards, in two asks: the button arms it on the server,
 * then the dialog sends back its token with the phrase typed out. The dialog
 * stays out of the polled page, so typing survives each refresh. */
const wipe = {
  dialog: null,
  asked: null, // The server's answer to the first ask, while the dialog is open.
  busy: false,
  result: null, // [text, tone] of the last attempt.

  async ask() {
    this.busy = true;
    this.result = null;
    render(lastData);
    try {
      this.asked = await send('POST', '/admin/api/maintenance/wipe-leaderboards');
      this.open();
    } catch (error) {
      this.result = [error.message, 'critical'];
    }
    this.busy = false;
    render(lastData);
  },

  open() {
    const { phrase, expires_at: expiresAt, impact } = this.asked;
    const input = h('input', { type: 'text', autocomplete: 'off', autocapitalize: 'off', spellcheck: 'false', 'aria-describedby': 'wipe-expires' });
    const confirm = h('button', { class: 'button destroy', type: 'submit', disabled: true }, 'Wipe leaderboards');
    input.addEventListener('input', () => {
      confirm.disabled = input.value.trim() !== phrase;
    });
    const form = h(
      'form',
      { onsubmit: (event) => { event.preventDefault(); this.confirm(input.value, confirm); } },
      h('h2', { id: 'wipe-title' }, 'Wipe all leaderboards?'),
      h(
        'p',
        null,
        'This deletes ',
        h('strong', null, plural(impact.entries, 'entry', 'entries')),
        ` of ${plural(impact.players, 'player', 'players')} on every board, and `,
        h('strong', null, plural(impact.past_entries, 'row', 'rows')),
        ' kept from past seasons. It cannot be undone.',
      ),
      onlineWarning(impact.online),
      h('label', { class: 'editor-field' }, h('span', null, 'Type ', h('code', null, phrase), ' to confirm'), input),
      h('p', { class: 'confirm-expires', id: 'wipe-expires' }, `This confirmation expires at ${clock.format(new Date(expiresAt))}.`),
      h(
        'div',
        { class: 'editor-actions' },
        h('button', { class: 'button', type: 'button', onclick: () => this.dialog.close() }, 'Cancel'),
        confirm,
      ),
    );
    if (!this.dialog) {
      this.dialog = h('dialog', { class: 'confirm', 'aria-labelledby': 'wipe-title' });
      this.dialog.addEventListener('close', () => {
        this.asked = null;
      });
      document.body.append(this.dialog);
    }
    this.dialog.replaceChildren(form);
    this.dialog.showModal();
    input.focus();
  },

  async confirm(phrase, button) {
    button.disabled = true;
    const { token } = this.asked;
    try {
      const wiped = await send('POST', '/admin/api/maintenance/wipe-leaderboards/confirm', { token, phrase });
      this.result = [`Wiped ${plural(wiped.scores, 'entry', 'entries')} and ${plural(wiped.past_scores, 'row', 'rows')} of past seasons.`, 'good'];
    } catch (error) {
      this.result = [error.message, 'critical'];
    }
    this.dialog.close();
    poll();
  },
};

// --- Planned sections ----------------------------------------------------

function renderPlanned() {
  const plan = section.planned;
  return [
    h(
      'section',
      { class: 'card planned' },
      h('span', { class: 'planned-icon' }, icon(section.icon)),
      badge(null, 'Planned'),
      h('h2', null, 'What you will be able to do here'),
      h('ul', null, plan.items.map((item) => h('li', null, item))),
      h('p', { class: 'planned-needs' }, `This needs ${plan.needs}, which the server does not run yet.`),
    ),
  ];
}

// --- Page ----------------------------------------------------------------

function renderNav() {
  const nav = document.getElementById('nav');
  let group;
  for (const target of SECTIONS) {
    if (target.group !== group) {
      group = target.group;
      if (group) nav.append(h('p', { class: 'nav-group' }, group));
    }
    nav.append(
      h(
        'a',
        {
          class: 'nav-item',
          href: target.path,
          'aria-current': target === section ? 'page' : null,
          'data-owner': target.owner,
          hidden: target.owner,
        },
        icon(target.icon),
        h('span', null, target.name),
        target.planned && h('span', { class: 'nav-soon' }, 'Soon'),
      ),
    );
  }
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
  for (const item of document.querySelectorAll('#nav [data-owner]')) item.hidden = !viewer.owner;
}

function renderFooter(data, now) {
  document.getElementById('footer').replaceChildren(
    `Version ${data.server.version} · Revision `,
    h('code', { title: data.server.revision }, data.server.revision.slice(0, 12)),
    ` · Server up ${duration(since(data.server.started_at, now))}`,
  );
}

function render(data) {
  const now = Date.parse(data.now);
  renderViewer(data.viewer);
  renderFooter(data, now);
  let body;
  if (section.planned) body = renderPlanned();
  else if (route.id) body = section.detail(data, now);
  else body = section.render(data, now);
  document.getElementById('body').replaceChildren(...body);
}

/** A row's page: its title and what it is, once its data is there. */
function setHead(title, intro) {
  document.getElementById('page-title').textContent = title;
  document.getElementById('page-intro').replaceChildren(...[intro].flat());
  document.title = `${title} · ${section.name} · Recharged WFC`;
}

function setLive(state, text) {
  document.getElementById('live').dataset.state = state;
  document.getElementById('live-text').textContent = text;
}

async function poll() {
  clearTimeout(timer);
  const asked = ++polls;
  try {
    const query = section.api === 'logs' ? logs.query() : '';
    const api = route.id ? `${section.api}/${route.id}` : section.api || 'overview';
    const response = await fetch(`/admin/api/${api}${query}`, {
      cache: 'no-store',
      headers: { Accept: 'application/json' },
    });
    if (response.status === 401) {
      // The session ended: the page itself now shows the sign-in.
      window.location.reload();
      return;
    }
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const data = await response.json();
    if (asked !== polls) return;
    if (section.api === 'database') sample(data);
    lastData = data;
    render(data);
    const at = clock.format(new Date(data.now));
    setLive(logs.paused ? 'paused' : 'live', logs.paused ? `Paused · ${at}` : `Live · ${at}`);
  } catch (error) {
    if (asked !== polls) return;
    setLive('error', 'Server unreachable · retrying');
  }
  if (!document.hidden && !logs.paused) timer = setTimeout(poll, POLL_MS);
}

document.title = `${section.name} · Recharged WFC`;
renderNav();
document.getElementById('content').replaceChildren(
  h(
    'header',
    { class: 'page-head' },
    h('span', { class: 'page-icon' }, icon(section.icon)),
    h(
      'div',
      null,
      route.id && h('a', { class: 'page-back', href: section.path }, `‹ ${section.name}`),
      h('h1', { id: 'page-title' }, route.id ? `${section.name} · ${route.id}` : section.name),
      h('p', { id: 'page-intro' }, route.id ? 'Loading…' : section.intro),
    ),
  ),
  h('div', { class: 'page-body', id: 'body' }, banner({ level: 'pending', title: 'Loading…', detail: 'Asking the server.' })),
);

document.addEventListener('visibilitychange', () => {
  if (document.hidden) {
    clearTimeout(timer);
    setLive('paused', 'Paused');
  } else {
    poll();
  }
});

poll();
