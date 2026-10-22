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

const ICONS = {
  overview: 'M3 3h7v7H3zM14 3h7v7h-7zM14 14h7v7h-7zM3 14h7v7H3z',
  players: 'M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2M9 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM22 21v-2a4 4 0 0 0-3-3.87M16 3.13a4 4 0 0 1 0 7.75',
  matchmaking: 'M5 12.55a11 11 0 0 1 14.08 0M1.42 9a16 16 0 0 1 21.16 0M8.53 16.11a6 6 0 0 1 6.95 0M12 20h.01',
  leaderboards: 'M6 9H4.5a2.5 2.5 0 0 1 0-5H6M18 9h1.5a2.5 2.5 0 0 0 0-5H18M4 22h16M10 14.66V17c0 .55-.47.98-.97 1.21C7.85 18.75 7 20.24 7 22M14 14.66V17c0 .55.47.98.97 1.21C16.15 18.75 17 20.24 17 22M18 2H6v7a6 6 0 0 0 12 0V2z',
  moderation: 'M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z',
  database: 'M12 8c4.97 0 9-1.34 9-3s-4.03-3-9-3-9 1.34-9 3 4.03 3 9 3zM21 12c0 1.66-4 3-9 3s-9-1.34-9-3M3 5v14c0 1.66 4 3 9 3s9-1.34 9-3V5',
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
    path: '/admin/leaderboards',
    group: 'Game',
    name: 'Leaderboards',
    icon: 'leaderboards',
    intro: 'gamestats2: the scores players put, and who leads each board.',
    api: 'leaderboards',
    render: renderLeaderboards,
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

const section = SECTIONS.find((candidate) => candidate.path === window.location.pathname) || SECTIONS[0];

let timer = null;
let lastData = null; // For re-rendering on a click, between polls.
let previous = null; // The last transaction counters.
let samples = []; // { at, rate } for the sparkline.
let hover = null; // The pointer's position across the sparkline, 0 to 1.
let category = 0; // The board whose leaders are shown.

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
  const body = section.planned ? renderPlanned() : section.render(data, now);
  document.getElementById('body').replaceChildren(...body);
}

function setLive(state, text) {
  document.getElementById('live').dataset.state = state;
  document.getElementById('live-text').textContent = text;
}

async function poll() {
  clearTimeout(timer);
  try {
    const response = await fetch(`/admin/api/${section.api || 'overview'}`, {
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
    if (section.api === 'database') sample(data);
    lastData = data;
    render(data);
    setLive('live', `Live · ${clock.format(new Date(data.now))}`);
  } catch (error) {
    setLive('error', 'Server unreachable · retrying');
  }
  if (!document.hidden) timer = setTimeout(poll, POLL_MS);
}

document.title = `${section.name} · Recharged WFC`;
renderNav();
document.getElementById('content').replaceChildren(
  h('header', { class: 'page-head' }, h('span', { class: 'page-icon' }, icon(section.icon)), h('div', null, h('h1', null, section.name), h('p', null, section.intro))),
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
