import {categories, movies, shorts, youtube, podcasts, picks} from './catalog.js';

const view = document.querySelector('#view');
const nav = document.querySelector('#nav');
const status = document.querySelector('#status');
const days = ['Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday', 'Sunday'];
const moods = ['Chill', 'Funny', 'Exciting', 'Thoughtful', 'Scary'];
const tools = [
  ['recommend', 'What should I watch?', 'spark', 'Five picks for your mood and time.'],
  ['movie-night', 'Movie night', 'film', 'Ten features and quick backups.'],
  ['music-station', 'Music station', 'music', 'A week of focus, gym, chill and night mixes.'],
  ['youtube-week', 'YouTube lineup', 'tv', 'Seven days of curated channels.'],
  ['weekend', 'Weekend planner', 'calendar', 'Build a lineup around your free time.'],
  ['podcast-radio', 'Podcast radio', 'podcast', 'Five listening days on one topic.'],
  ['smart-content', 'Smart content', 'learn', 'A week of learning in short sessions.'],
];

const iconPaths = {
  home: 'M3 10l9-7 9 7v10H3z M9 20v-7h6v7',
  film: 'M3 4h18v16H3z M7 4v16 M17 4v16 M3 9h4m-4 6h4m10-6h4m-4 6h4',
  tv: 'M3 6h18v13H3z M8 2l4 4 4-4 M8 22h8',
  music: 'M9 18V5l12-3v13 M9 8l12-3 M9 18a3 2 0 1 1-6 0 3 2 0 1 1 6 0 M21 15a3 2 0 1 1-6 0 3 2 0 1 1 6 0',
  podcast: 'M9 14a4 4 0 1 1 6 0 M6 17a8 8 0 1 1 12 0 M10 16h4l-1 6h-2z',
  book: 'M12 6C9 3 5 3 2 4v15c4-1 7-1 10 2 3-3 6-3 10-2V4c-3-1-7-1-10 2z M12 6v15',
  game: 'M7 8h10a4 4 0 0 1 4 3l1 5c1 4-3 5-5 2l-2-2H9l-2 2c-2 3-6 2-5-2l1-5a4 4 0 0 1 4-3z M5 12h6m-3-3v6m8-3h1m2 2h1',
  globe: 'M22 12a10 10 0 1 1-20 0 10 10 0 0 1 20 0z M2 12h20 M12 2c5 5 5 15 0 20-5-5-5-15 0-20z',
  learn: 'M2 8l10-5 10 5-10 5z M6 10v7c4 3 8 3 12 0v-7 M22 8v8',
  spark: 'M12 2c0 6 4 10 10 10-6 0-10 4-10 10 0-6-4-10-10-10 6 0 10-4 10-10z',
  calendar: 'M3 5h18v16H3z M7 2v6m10-6v6M3 10h18',
};
const icon = name => `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round" stroke-linecap="round" aria-hidden="true"><path d="${iconPaths[name]}"/></svg>`;
const slug = name => name.toLowerCase().replaceAll(' ', '-');
const escape = value => String(value).replace(/[&<>"']/g, char => ({
  '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
})[char]);
const search = (base, phrase) => base + encodeURIComponent(phrase);
const youtubeSearch = phrase => search('https://www.youtube.com/results?search_query=', phrase);
const podcastSearch = phrase => search('https://podcastaddict.com/search?q=', phrase);
const read = key => {
  try { return JSON.parse(localStorage.getItem(key)) ?? []; }
  catch { return []; }
};
const write = (key, value) => localStorage.setItem(key, JSON.stringify(value));
let saved = read('entertainment-saved');
let recent = read('entertainment-recent');
let entries = [];
let activePage = '';
const formValues = new Map();

function link(url, title, label = title) {
  return `<a class="external" href="${escape(url)}" data-title="${escape(title)}" target="_blank" rel="noopener noreferrer">${escape(label)} <span aria-hidden="true">↗</span></a>`;
}

function card({title, source, minutes, why, url}) {
  const index = entries.push({title, source, minutes, why, url}) - 1;
  const selected = saved.some(entry => entry.title === title && entry.url === url);
  return `<article class="card">
    <div class="card-top"><span>${escape(source)}${minutes ? ` · ${minutes} min` : ''}</span><button type="button" class="save" data-save="${index}" aria-pressed="${selected}" aria-label="Save ${escape(title)}">${selected ? '★' : '☆'}</button></div>
    <h3>${escape(title)}</h3><p>${escape(why)}</p>${link(url, title, 'Open provider')}
  </article>`;
}

const section = (title, html) => `<section><h2>${escape(title)}</h2>${html}</section>`;
const grid = html => `<div class="grid">${html}</div>`;
const heading = (title, copy) => `<h1>${escape(title)}</h1><p class="intro">${escape(copy)}</p>`;

function home() {
  const browse = categories.map(([name, symbol, copy]) =>
    `<a class="tile ${symbol}" href="#${slug(name)}"><span class="tile-icon">${icon(symbol)}</span><strong>${name}</strong><small>${copy}</small><span class="tile-arrow" aria-hidden="true">↗</span></a>`).join('');
  const planners = tools.map(([id, name, symbol, copy]) =>
    `<a class="tool-tile" href="#${id}">${icon(symbol)}<span><strong>${name}</strong><small>${copy}</small></span><span aria-hidden="true">›</span></a>`).join('');
  return `<section class="hero"><div class="hero-content"><p class="eyebrow">THE ENTERTAINMENT HUB</p><h1>Make tonight<br>worth watching.</h1><p>Free movies, music, games and more. Find something for right now or plan your whole week.</p><div class="hero-actions"><a class="primary" href="#recommend">${icon('spark')} Find my picks</a><a class="secondary" href="#movie-night">Plan movie night</a></div></div></section>
    ${section('Explore your library', `<div class="tiles">${browse}</div>`)}
    ${section('Make a plan', `<div class="tools">${planners}</div>`)}`;
}

function categoryPage([name, , description, providers]) {
  return heading(name, description) + grid(providers.map(([title, url, source, why]) =>
    card({title, url, source, why})).join(''));
}

const select = (label, name, values) => `<label>${label}<select name="${name}">${values.map(value =>
  `<option>${value}</option>`).join('')}</select></label>`;
const input = (label, name, placeholder) => `<label>${label}<input name="${name}" maxlength="120" placeholder="${placeholder}"></label>`;
const form = (title, copy, fields, action) => heading(title, copy) +
  `<form><div class="fields">${fields}</div><button class="primary">${action}</button></form><div id="results" aria-live="polite"></div>`;
const textTopic = (label = 'Interests') => input(label, 'topic', 'Science fiction, AI, technology');

const views = {
  recommend: () => form('What should I watch?', 'Get one free pick in each format. Each option fits your time on its own.',
    select('Mood', 'mood', moods) + select('Time available', 'time', ['15 minutes', '30 minutes', '60 minutes', '2 hours', '3 hours']) +
    select('Energy', 'energy', ['Low', 'Medium', 'High']) + select('Watching with', 'company', ['Just me', 'Friends', 'Family']) +
    textTopic(), 'Find my picks'),
  'movie-night': () => form('Movie night', 'Ten free features ranked for your mood, with quick backups from Blender Studio.',
    select('Mood', 'mood', moods) + textTopic('Favorite genres'), 'Plan my movie night'),
  'music-station': () => form('Music station', 'Build four listening playlists a day from free streams and official uploads.',
    input('Artists or genres', 'topic', 'Ambient, house, indie rock'), 'Build my week'),
  'youtube-week': () => heading('YouTube lineup', 'A seven-day schedule built around official channels. Use the channel link to confirm the uploader.') +
    schedule(youtube.map(([category, channel, phrase, minutes, why, url], i) => [
      days[i], minutes, category, why, link(url, channel) + link(search(url + '/search?query=', phrase), phrase),
    ])),
  weekend: () => form('Weekend planner', 'Split your time across Saturday and Sunday with something to listen to, watch, play and learn.',
    '<label>Hours available across the weekend<input type="number" name="hours" value="4" min="1" max="24" step="0.25" required></label>' +
    textTopic(), 'Plan my weekend'),
  'podcast-radio': () => form('Podcast radio', 'Explore one topic through five episode formats. Pick freely playable episodes within the suggested listening blocks.',
    textTopic('Topic'), 'Build my listening plan'),
  'smart-content': () => form('Smart content', 'Build a learning week with sessions under 45 minutes.',
    textTopic(), 'Build my learning week'),
  saved: () => heading('My list', 'Your saved providers and titles on this device.') +
    (saved.length ? grid(saved.map(card).join('')) : '<p class="empty">Use ☆ on a provider or pick to save it here.</p>') +
    section('Recently opened', recent.length ? `<ul class="recent">${recent.map(({url, title}) =>
      `<li>${link(url, title)}</li>`).join('')}</ul>` : '<p class="empty">Provider links you open will appear here.</p>'),
};

function schedule(rows) {
  return `<div class="schedule">${rows.map(([day, minutes, title, description, links]) =>
    `<article class="slot"><div><strong>${day}</strong>${minutes ? `<small>${minutes} min</small>` : ''}</div>
      <div><h3>${escape(title)}</h3><p>${escape(description)}</p><div class="slot-links">${links}</div></div></article>`).join('')}</div>`;
}

const terms = topic => topic.toLowerCase().match(/[\p{L}\p{N}]+/gu)?.filter(word => word.length > 1) ?? [];
const matches = (description, topic) => {
  const words = terms(description);
  return terms(topic).filter(word => words.includes(word)).length;
};

function rank(items, {topic = '', mood, energy, company, limit = Infinity}) {
  const score = entry => 3 * (entry.mood === mood) + 2 * (entry.energy === energy) +
    2 * matches(entry.tags + ' ' + entry.title, topic) +
    (company === 'Friends' && ['Funny', 'Exciting'].includes(entry.mood) ? 1 : 0);
  return items.filter(entry => entry.minutes <= limit && (company !== 'Family' || entry.family))
    .sort((a, b) => score(b) - score(a) || (energy === 'Low' ? a.minutes - b.minutes : b.minutes - a.minutes));
}

function musicPlan(topic) {
  const focus = ['instrumental mix', 'ambient session', 'lo-fi beats', 'minimal piano mix', 'deep focus electronic', 'instrumental jazz', 'study session'];
  const gym = ['DJ workout mix', 'high energy mix', 'running set', 'club session', 'cardio mix', 'uptempo remixes', 'training mix'];
  const chill = ['acoustic session', 'downtempo', 'unplugged live', 'mellow indie', 'soul live session', 'Sunday radio mix', 'soft grooves'];
  const night = ['ambient', 'drone', 'soft piano', 'night jazz', 'slow instrumental', 'sleep ambient', 'minimal synth'];
  return schedule(days.map((day, i) => {
    const queries = [`${topic} ${focus[i]} official`, `${topic} ${gym[i]}`, `${topic} ${chill[i]} official`, `${topic} ${night[i]}`];
    const stations = [
      ['Focus · YouTube', youtubeSearch(queries[0])],
      ['Gym · SoundCloud', search('https://soundcloud.com/search/sounds?q=', queries[1])],
      ['Chill · YouTube', youtubeSearch(queries[2])],
      ['Night · Bandcamp', search('https://bandcamp.com/search?q=', queries[3])],
    ];
    return [day, null, 'Four playlist starters', 'Save the tracks you like in your activity queues on each platform.',
      stations.map(([label, url], j) => link(url, queries[j], label + ' · ' + queries[j])).join('')];
  }));
}

function weekendPlan(topic, totalMinutes) {
  const blocks = 6;
  const base = Math.floor(totalMinutes / blocks);
  return schedule(['Saturday morning', 'Saturday afternoon', 'Saturday night', 'Sunday morning', 'Sunday afternoon', 'Sunday night'].map((day, i) => {
    const minutes = base + Number(i < totalMinutes % blocks);
    const options = [
      ['Podcast', `Pick a freely playable episode or listen for ${minutes} minutes.`, podcastSearch(topic + ' beginner guide')],
      ['Game', 'Play a few rounds of 2048. Take breaks or replay if you have time left.', 'https://gabrielecirulli.github.io/2048/'],
      ['Movie', '', ''],
      ['Learning', 'Open an official lesson and write down one thing you can explain afterward.', youtubeSearch(topic + ' introductory lesson university')],
      ['Podcast', `Try an interview on ${topic}, using your time as a listening cap.`, podcastSearch(topic + ' expert interview')],
      ['Documentary', '', ''],
    ];
    let [title, copy, url] = options[i];
    if (i === 2 || i === 5) {
      const pool = i === 2 ? [...movies, ...shorts] : picks.Documentary;
      const entry = rank(pool, {topic, limit: minutes})[0];
      if (entry) {
        title = entry.title;
        copy = `${entry.why} ${entry.minutes} min on ${entry.source}.`;
        url = entry.url;
      } else {
        copy = 'Watch the opening segment on NASA+ and save the rest for later.';
        url = 'https://plus.nasa.gov/';
      }
    }
    return [day, minutes, title, copy, link(url, title, 'Open provider')];
  }));
}

function learningPlan(topic) {
  const lessons = [
    ['Core vocabulary', 'beginner introduction', 20, 'Define three basic terms.'],
    ['How it works', 'how it works fundamentals', 25, 'Connect the core terms in a simple model.'],
    ['A real example', 'real world application case study', 30, 'Explain one practical use and its limitations.'],
    ['Hands-on practice', 'beginner hands on exercise', 35, 'Try one small exercise and explain the result.'],
    ['Common mistakes', 'common mistakes misconceptions', 25, 'Recognize two common errors and how to avoid them.'],
    ['Compare approaches', 'approaches comparison lecture', 40, 'Describe when you would choose each approach.'],
    ['Review', 'recap quiz explained', 20, 'Summarize the subject from memory and identify one gap.'],
  ];
  return schedule(lessons.map(([title, query, minutes, goal], i) => {
    const phrase = `${topic} ${query}`;
    return [days[i], minutes, title, goal + ' Use one lesson or a segment that fits the time box.',
      link(youtubeSearch(phrase), phrase) + link('https://ocw.mit.edu/search/?q=' + encodeURIComponent(topic), 'MIT OpenCourseWare')];
  }));
}

function results(page, data) {
  const topic = data.topic.trim() || ({
    'music-station': 'electronic', 'podcast-radio': 'space exploration', 'smart-content': 'AI', weekend: 'science fiction space',
  })[page] || '';
  if (page === 'recommend') {
    const hours = data.time.includes('hours');
    const limit = parseInt(data.time, 10) * (hours ? 60 : 1);
    const formats = {'Movie': [...movies, ...shorts], ...picks};
    return `<div class="grid recommendations">${Object.entries(formats).map(([name, items]) => {
      const entry = rank(items, {...data, topic, limit})[0];
      return `<section><h2>${name}</h2>${card(entry)}</section>`;
    }).join('')}</div>`;
  }
  if (page === 'movie-night') return section('Your 10 movie picks', grid(rank(movies, {...data, topic}).map(card).join(''))) +
    section('Quick backups · free short films', grid(rank(shorts, {...data, topic}).slice(0, 3).map(card).join('')));
  if (page === 'music-station') return section(`Your week of ${topic}`, musicPlan(topic));
  if (page === 'weekend') return weekendPlan(topic, Math.round(Number(data.hours) * 60));
  if (page === 'smart-content') return section(`A week of ${topic}`, learningPlan(topic));
  return section(`Five days of ${topic}`, schedule(podcasts.map(([type, query, why, minutes], i) => {
    const phrase = `${topic} ${query}`;
    return [days[i], minutes, type, why, link(podcastSearch(phrase), phrase)];
  })));
}

function render() {
  const page = location.hash.slice(1) || 'home';
  const category = categories.find(([name]) => slug(name) === page);
  const tool = tools.find(([id]) => id === page);
  const label = category?.[0] || tool?.[1] || (page === 'saved' ? 'My list' : 'Home');
  const navLink = (id, name, symbol) =>
    `<a href="#${id}"${page === id ? ' aria-current="page"' : ''}>${icon(symbol)}${name}</a>`;
  nav.innerHTML = navLink('home', 'Home', 'home') + '<p>LIBRARY</p>' +
    categories.map(([name, symbol]) => navLink(slug(name), name, symbol)).join('') +
    '<p>PLANNERS</p>' + tools.map(([id, name, symbol]) => navLink(id, name, symbol)).join('');
  document.querySelector('#breadcrumb').textContent = label;
  entries = [];
  view.innerHTML = category ? categoryPage(category) : Object.hasOwn(views, page) ? views[page]() : home();
  if (formValues.has(page)) {
    const data = formValues.get(page);
    for (const [key, value] of Object.entries(data)) view.querySelector(`[name="${key}"]`).value = value;
    view.querySelector('#results').innerHTML = results(page, data);
  }
  if (activePage !== page) window.scrollTo(0, 0);
  activePage = page;
}

view.addEventListener('submit', event => {
  event.preventDefault();
  const data = Object.fromEntries(new FormData(event.target));
  formValues.set(activePage, data);
  entries = [];
  view.querySelector('#results').innerHTML = results(activePage, data);
});

view.addEventListener('click', async event => {
  const button = event.target.closest('[data-save]');
  if (button) {
    const entry = entries[Number(button.dataset.save)];
    const isSaved = saved.some(other => entry.title === other.title && entry.url === other.url);
    saved = isSaved ? saved.filter(other => other.title !== entry.title || other.url !== entry.url) : [...saved, entry];
    write('entertainment-saved', saved);
    status.textContent = isSaved ? 'Removed from My list.' : 'Saved to My list.';
    if (activePage === 'saved') render();
    else {
      button.setAttribute('aria-pressed', String(!isSaved));
      button.textContent = isSaved ? '☆' : '★';
    }
    return;
  }
  const anchor = event.target.closest('a[data-title]');
  if (!anchor) return;
  const entry = {title: anchor.dataset.title, url: anchor.href};
  if (window.__TAURI__) {
    event.preventDefault();
    try { await window.__TAURI__.core.invoke('open_provider', {url: entry.url}); }
    catch (error) {
      status.textContent = `Could not open provider. ${error}`;
      return;
    }
  }
  recent = [entry, ...recent.filter(other => other.url !== entry.url || other.title !== entry.title)].slice(0, 10);
  write('entertainment-recent', recent);
  if (activePage === 'saved') render();
});

window.addEventListener('hashchange', render);
render();
