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
// A match's state (docs/profiles.md, "Matches"), with its badge tone.
const MATCH_STATES = {
  challenged: ['Challenged', 'warning'],
  accepted: ['Accepted', 'accent'],
  finished: ['Finished', 'good'],
  disputed: ['Disputed', 'serious'],
  declined: ['Declined', null],
  cancelled: ['Cancelled', null],
  expired: ['Expired', null],
};

const ICONS = {
  overview: 'M3 3h7v7H3zM14 3h7v7h-7zM14 14h7v7h-7zM3 14h7v7H3z',
  players: 'M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2M9 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM22 21v-2a4 4 0 0 0-3-3.87M16 3.13a4 4 0 0 1 0 7.75',
  matchmaking: 'M5 12.55a11 11 0 0 1 14.08 0M1.42 9a16 16 0 0 1 21.16 0M8.53 16.11a6 6 0 0 1 6.95 0M12 20h.01',
  matches: 'M7 16V4M3 8l4-4 4 4M17 8v12M21 16l-4 4-4-4',
  leaderboards: 'M6 9H4.5a2.5 2.5 0 0 1 0-5H6M18 9h1.5a2.5 2.5 0 0 0 0-5H18M4 22h16M10 14.66V17c0 .55-.47.98-.97 1.21C7.85 18.75 7 20.24 7 22M14 14.66V17c0 .55.47.98.97 1.21C16.15 18.75 17 20.24 17 22M18 2H6v7a6 6 0 0 0 12 0V2z',
  competitive: 'M4 22v-7M4 15V3h13l-3 6 3 6H4',
  moderation: 'M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z',
  database: 'M12 8c4.97 0 9-1.34 9-3s-4.03-3-9-3-9 1.34-9 3 4.03 3 9 3zM21 12c0 1.66-4 3-9 3s-9-1.34-9-3M3 5v14c0 1.66 4 3 9 3s9-1.34 9-3V5',
  logs: 'M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8zM14 2v6h6M16 13H8M16 17H8M10 9H8',
  good: 'M20 6 9 17l-5-5',
  warning: 'M12 9v4m0 4h.01M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z',
  serious: 'M12 9v4m0 4h.01M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z',
  critical: 'M18 6 6 18M6 6l12 12',
  pending: 'M12 6v6l4 2',
};

// The sections, in the order of the server's PAGES (src/admin/mod.rs). A
// planned section has no data of its own yet: it polls the overview, for
// the header and footer.
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
let polls = 0; // Polls started, so a slow answer never replaces a newer one.

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
  const planned = SECTIONS.filter((target) => target.planned).length;
  const live = SECTIONS.length - 1 - planned;
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
  const targets = SECTIONS.filter((target) => target.path !== '/admin');
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
  return [badge(tone, name), h('span', { class: 'sub' }, ['since ', ago(player.online.since, now)])];
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
          { label: 'Player', cell: (p) => [p.mii_name || 'Unnamed', h('span', { class: 'sub' }, `Profile ${p.profile_id}`)] },
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
  return [state, h('span', { class: 'sub' }, `Cookie ${event.cookie} · ${event.reports.length} of 2 reports`)];
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

/** A team in the list: its Mii, bold when it won, and its captain. */
function matchTeam(team, won) {
  const name = won ? h('strong', null, team.mii) : team.mii;
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

function matchBanner(match, now) {
  const times = match.ended_at
    ? `Ended ${ago(match.ended_at, now).textContent} · took ${duration(since(match.started_at, now) - since(match.ended_at, now))}`
    : `Last report ${ago(match.last_report_at, now).textContent}`;
  if (match.state === 'finished') return banner({ level: 'good', title: matchResult(match), detail: times });
  if (match.state === 'disputed') {
    return banner({ level: 'warning', title: 'Disputed', detail: `The two games saw a game differently · ${times}` });
  }
  if (match.state === 'abandoned') {
    return banner({ level: 'serious', title: 'Abandoned', detail: `The series stopped before a side won it · ${times}` });
  }
  return banner({ level: 'pending', title: `In progress, ${match.score.home} – ${match.score.away}`, detail: times });
}

function sideReporter(side) {
  const reporter = side.reporter;
  if (!reporter) return h('span', { class: 'sub' }, 'No reports: a retail game, or its reports never came');
  return [
    `Profile ${reporter.profile_id}`,
    h('span', { class: 'sub' }, ['Friend code ', h('code', null, reporter.friend_code)]),
  ];
}

function sideEnd(side, now) {
  const reporter = side.reporter;
  if (!reporter) return '—';
  if (!reporter.reason) return [badge(null, 'No end yet'), h('span', { class: 'sub' }, ['last report ', ago(reporter.last_report_at, now)])];
  const [name, tone] = reporter.reason === 'decided' ? ['Decided', 'good'] : ['Abandoned', null];
  return [badge(tone, name), h('span', { class: 'sub' }, ago(reporter.ended_at, now))];
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
    matchBanner(match, now),
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
          { label: 'Ending', cell: (row) => gameCell(row, 'ending', (ending) => ENDINGS[ending] || ending) },
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
    'The latest test matches between players who signed in.',
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

  async send(method, path, body) {
    const response = await fetch(path, {
      method,
      headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
      body: body && JSON.stringify(body),
    });
    if (response.status === 401) window.location.reload();
    const answer = response.status === 204 ? null : await response.json().catch(() => null);
    if (!response.ok) throw new Error((answer && answer.error) || `HTTP ${response.status}`);
    return answer;
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
      const saved = await this.send(this.editing ? 'PUT' : 'POST', path, form);
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
      await this.send('DELETE', `/admin/api/rules/${this.editing}`);
      this.render(null);
      this.status('Deleted', 'good');
      poll();
    } catch (error) {
      this.status(error.message, 'critical');
    }
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
        { class: 'nav-item', href: target.path, 'aria-current': target === section ? 'page' : null },
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
