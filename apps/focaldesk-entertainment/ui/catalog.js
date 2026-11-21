export const categories = [
  ['Movies', 'film', 'Friday night features', [
    ['Tubi', 'https://tubitv.com/category/movies', 'Ad-supported feature films', 'Save your shortlist in My List.'],
    ['Pluto TV', 'https://pluto.tv/', 'Live movie channels', 'Keep a favorite channel for drop-in viewing.'],
    ['Plex', 'https://watch.plex.tv/', 'Free on-demand films', 'Choose Watch Free and add titles to your Watchlist.'],
    ['Kanopy', 'https://www.kanopy.com/', 'Indie and classic cinema', 'Connect a participating library card and track monthly tickets.'],
    ['Hoopla', 'https://www.hoopladigital.com/', 'Library-supported movies', 'Browse movies and reserve a borrow for movie night.'],
  ]],
  ['TV Shows', 'tv', 'Find your next series', [
    ['Tubi', 'https://tubitv.com/category/tv', 'Classic and complete series', 'Save a series and resume where you left off.'],
    ['Pluto TV', 'https://pluto.tv/', 'Channel surfing', 'Favorite channels and check their program guide.'],
    ['The Roku Channel', 'https://therokuchannel.roku.com/', 'Free shows and originals', 'Stick to the free rows and save a series.'],
    ['Plex', 'https://watch.plex.tv/live-tv', 'Live TV channels', 'Use the guide to schedule an evening block.'],
    ['PBS', 'https://www.pbs.org/video/', 'Public television', 'Pick episodes labeled as freely available.'],
  ]],
  ['Music', 'music', 'Build your own station', [
    ['YouTube', 'https://www.youtube.com/music', 'Official videos and live sessions', 'Save official uploads into four activity playlists.'],
    ['SoundCloud', 'https://soundcloud.com/discover', 'Indie artists and DJ mixes', 'Follow artists and save tracks available to stream free.'],
    ['Bandcamp', 'https://bandcamp.com/discover', 'Independent releases', 'Follow labels and stream tracks enabled by the artist.'],
    ['Jamendo', 'https://www.jamendo.com/', 'Independent music discovery', 'Start with genre mixes and bookmark artists you like.'],
    ['SomaFM', 'https://somafm.com/', 'Curated internet radio', 'Pin one station each for work and winding down.'],
  ]],
  ['Podcasts', 'podcast', 'Make time to listen', [
    ['AntennaPod', 'https://antennapod.org/', 'Open-source Android player', 'Subscribe by RSS and queue episodes for offline listening.'],
    ['Pocket Casts', 'https://pocketcasts.com/', 'Episode organization', 'Use the free mobile app and build an Up Next queue.'],
    ['Spotify', 'https://open.spotify.com/genre/podcasts-web', 'Free public podcasts', 'Follow public feeds and save episodes for later.'],
    ['Apple Podcasts', 'https://podcasts.apple.com/', 'Browsing and discovery', 'Follow free shows and skip subscriber-only episodes.'],
    ['Podcast Addict', 'https://podcastaddict.com/', 'Android playback controls', 'Set playback speed and create an automatic queue.'],
  ]],
  ['Books', 'book', 'Read or listen anywhere', [
    ['Project Gutenberg', 'https://www.gutenberg.org/', 'Public-domain classics', 'Download EPUBs for an offline reading shelf.'],
    ['Libby', 'https://libbyapp.com/', 'Library ebooks and audiobooks', 'Add a participating card and manage holds.'],
    ['Open Library', 'https://openlibrary.org/', 'Digital lending and open editions', 'Use Read and Borrow filters to find an available edition.'],
    ['Standard Ebooks', 'https://standardebooks.org/ebooks', 'Carefully formatted classics', 'Download the format that fits your reader.'],
    ['LibriVox', 'https://librivox.org/', 'Volunteer-read audiobooks', 'Download a complete recording or subscribe to its feed.'],
  ]],
  ['Games', 'game', 'Play without a subscription', [
    ['Steam', 'https://store.steampowered.com/genre/Free%20to%20Play/', 'Free-to-play PC games', 'Make a free games collection in your library.'],
    ['itch.io', 'https://itch.io/games/free', 'Indie and browser games', 'Filter by HTML5 for quick sessions.'],
    ['GOG', 'https://www.gog.com/en/partner/free_games', 'Free DRM-free titles', 'Claim the free games you want in your library.'],
    ['Epic Games Store', 'https://store.epicgames.com/en-US/free-games', 'Rotating giveaways', 'Check for titles marked Free Now.'],
    ['0 A.D.', 'https://play0ad.com/', 'Open-source strategy', 'Start with a solo skirmish.'],
  ]],
  ['Documentaries', 'globe', 'Go deeper on a subject', [
    ['PBS NOVA', 'https://www.pbs.org/wgbh/nova/', 'Science documentaries', 'Build a theme night around available full episodes.'],
    ['NASA+', 'https://plus.nasa.gov/', 'Space missions and science', 'Queue documentaries and check the live event schedule.'],
    ['DW Documentary', 'https://www.youtube.com/@DWDocumentary', 'International affairs', 'Use the official channel playlists for recurring topics.'],
    ['FRONTLINE', 'https://www.pbs.org/wgbh/frontline/', 'Investigative reporting', 'Pair a film with its companion reporting.'],
    ['National Film Board of Canada', 'https://www.nfb.ca/documentary/', 'Canadian and independent nonfiction', 'Look for titles with free playback in your region.'],
  ]],
  ['Learning', 'learn', 'Spend your screen time well', [
    ['MIT OpenCourseWare', 'https://ocw.mit.edu/', 'University course materials', 'Pair a lecture segment with one practice problem.'],
    ['freeCodeCamp', 'https://www.freecodecamp.org/learn/', 'Hands-on programming', 'Save progress and work one module at a time.'],
    ['Khan Academy', 'https://www.khanacademy.org/', 'Math and science foundations', 'Use practice exercises to test what you remember.'],
    ['Harvard CS50', 'https://cs50.harvard.edu/x/', 'Computer science fundamentals', 'Use the free course site and budget time for problem sets.'],
    ['Microsoft Learn', 'https://learn.microsoft.com/training/', 'Cloud and developer skills', 'Choose a free module and review requirements for labs.'],
  ]],
];

const tubi = title => 'https://tubitv.com/search/' + encodeURIComponent(title);
const blender = 'https://studio.blender.org/films/';
const item = (title, minutes, mood, energy, family, source, url, why, tags = '') =>
  ({title, minutes, mood, energy, family, source, url, why, tags});

export const movies = [
  item('Coraline', 101, 'Scary', 'Medium', true, 'Tubi · PG', tubi('Coraline'), 'Stop-motion whimsy turns quietly sinister.', 'fantasy animation'),
  item('Central Intelligence', 108, 'Funny', 'High', false, 'Tubi · PG-13', tubi('Central Intelligence'), 'An odd-couple buddy comedy with big action beats.'),
  item('The Perfect Host', 93, 'Scary', 'Medium', false, 'Tubi · R', tubi('The Perfect Host'), 'A dinner party keeps shifting the balance of power.'),
  item('Battle: Los Angeles', 117, 'Exciting', 'High', false, 'Tubi · PG-13', tubi('Battle: Los Angeles'), 'An alien invasion plays out at street level.', 'science fiction space'),
  item('Plan 9 From Outer Space', 79, 'Funny', 'Low', false, 'Tubi · TV-14', 'https://tubitv.com/movies/624923/plan-9-from-outer-space', 'Campy flying saucers and wonderfully strange dialogue.', 'science fiction space'),
  item('3022', 92, 'Thoughtful', 'Medium', false, 'Tubi · R', tubi('3022'), 'A space-station crew must rethink its purpose after disaster.', 'science fiction space technology'),
  item('RiP! A Remix Manifesto', 87, 'Thoughtful', 'Medium', false, 'NFB', 'https://www.nfb.ca/film/rip_a_remix_manifesto/', 'A feature documentary about sampling and who gets to remix culture.', 'music copyright technology'),
  item('The Darkest Hour', 89, 'Exciting', 'High', false, 'Tubi · PG-13', tubi('The Darkest Hour'), 'An invisible threat makes a deserted city feel dangerous.', 'science fiction aliens'),
  item('The Magnificent Seven', 133, 'Exciting', 'High', false, 'Tubi · PG-13', tubi('The Magnificent Seven'), 'An ensemble Western built around a classic defend-the-town setup.'),
  item('The House', 89, 'Funny', 'Medium', false, 'Tubi · R', tubi('The House'), 'A suburban casino premise keeps escalating into absurdity.'),
];
export const shorts = [
  item('Wing It!', 5, 'Funny', 'High', true, 'Blender Studio', blender, 'An out-of-control spaceship makes for a quick slapstick ride.', 'space animation'),
  item('Big Buck Bunny', 10, 'Funny', 'Low', true, 'Blender Studio', blender, 'A gentle giant turns the tables on some woodland troublemakers.', 'animation'),
  item('Spring', 8, 'Chill', 'Low', true, 'Blender Studio', blender, 'A wordless fantasy with a shepherd, a dog and ancient spirits.', 'animation nature'),
  item('Sprite Fright', 11, 'Scary', 'Medium', false, 'Blender Studio', blender, 'A colorful forest comedy takes a gruesome turn.', 'animation horror'),
  item('Charge', 4, 'Exciting', 'High', false, 'Blender Studio', blender, 'A compact sci-fi chase in a world desperate for energy.', 'science fiction technology'),
  item('Tears of Steel', 12, 'Thoughtful', 'Medium', false, 'Blender Studio', blender, 'A live-action open film about robots, old regrets and saving a city.', 'science fiction AI technology'),
];

export const youtube = [
  ['Space & science', 'NASA', 'NASA Artemis I documentary', 35, 'See how an uncrewed mission tested the Orion spacecraft.', 'https://www.youtube.com/@NASA', 'space astronomy technology'],
  ['Visual explainers', '3Blue1Brown', '3Blue1Brown But what is a neural network', 20, 'Build an intuition for how a neural network processes inputs.', 'https://www.youtube.com/@3blue1brown', 'math AI technology'],
  ['History', 'Crash Course', 'Crash Course World History Agricultural Revolution', 15, 'Trace how farming transformed social organization.', 'https://www.youtube.com/@crashcourse', 'history society'],
  ['Live music', 'KEXP', 'KEXP Khruangbin full performance', 35, 'Hear how a band rearranges songs for a studio session.', 'https://www.youtube.com/@kexp', 'music live'],
  ['Engineering', 'Practical Engineering', 'Practical Engineering What is a Watershed', 20, 'See how terrain shapes water flow.', 'https://www.youtube.com/@PracticalEngineeringChannel', 'engineering nature technology'],
  ['Animation', 'Blender Studio', 'Blender Studio Big Buck Bunny', 10, 'Watch a complete open animated short.', 'https://www.youtube.com/@BlenderStudio', 'art animation comedy'],
  ['Economics', 'Crash Course', 'Crash Course Economics Intro to Economics', 15, 'Learn how incentives and scarcity shape everyday choices.', 'https://www.youtube.com/@crashcourse', 'business economics money'],
];

export const podcasts = [
  ['Primer', 'explained beginner guide', 'Learn the terms people use when they discuss the topic.', 15],
  ['Interview', 'expert interview', 'Hear how a practitioner thinks through real decisions.', 30],
  ['Case study', 'case study story', 'Follow one concrete example from problem to outcome.', 25],
  ['Debate', 'debate critique', 'Compare competing views and the evidence behind them.', 35],
  ['Practical recap', 'practical lessons takeaways', 'Turn the week’s ideas into something you can test.', 20],
];

export const picks = {
  'YouTube video': [
    item('Big Buck Bunny', 10, 'Funny', 'Low', true, 'Blender', 'https://www.youtube.com/watch?v=aqz-KE-bpKQ', 'An animated short with big visual gags.', 'comedy animation'),
    item('But what is a neural network?', 20, 'Thoughtful', 'Medium', true, '3Blue1Brown', 'https://www.youtube.com/watch?v=aircAruvnKk', 'A visual introduction to the building blocks of a neural network.', 'AI math technology'),
  ],
  Podcast: [
    item('Short Wave', 15, 'Thoughtful', 'Low', true, 'NPR · choose an episode under 15 min', 'https://www.npr.org/podcasts/510351/short-wave', 'Short science stories for a quick listening session.', 'science technology space'),
    item('Curiosity Daily', 15, 'Chill', 'Low', true, 'Curiosity Daily · choose an episode under 15 min', 'https://curiosity-daily.simplecast.com/', 'Easy-to-dip-into stories about everyday science.', 'science technology'),
  ],
  Game: [
    item('2048', 10, 'Chill', 'Low', true, 'Browser', 'https://gabrielecirulli.github.io/2048/', 'Set a timer and see how far you can get by merging tiles.', 'puzzle math'),
    item('lichess', 15, 'Thoughtful', 'Medium', true, 'Browser', 'https://lichess.org/', 'Try the daily puzzle or set up a quick match with a friend.', 'chess strategy'),
    item('Hextris', 10, 'Exciting', 'High', true, 'Browser', 'https://hextris.io/', 'A fast, colorful browser puzzle you can stop at any time.', 'arcade puzzle'),
  ],
  Documentary: [
    item('Moon Base: Lunar Rovers', 10, 'Thoughtful', 'Low', true, 'NASA+', 'https://plus.nasa.gov/video/moon-base-update-lunar-rovers/', 'See the vehicles NASA is developing for the lunar surface.', 'space science engineering technology'),
    item('Our Alien Earth: Greenland', 16, 'Chill', 'Low', true, 'NASA+', 'https://plus.nasa.gov/video/our-alien-earth-greenland/', 'Follow research in an extreme environment that helps scientists think about other worlds.', 'nature space science'),
    item('Artemis I: The Documentary', 35, 'Exciting', 'Medium', true, 'NASA+', 'https://plus.nasa.gov/video/artemis-i-the-documentary/', 'Go behind the scenes of a Moon mission.', 'science space technology'),
    item('The Color of Space', 51, 'Thoughtful', 'Medium', true, 'NASA+', 'https://plus.nasa.gov/series/the-color-of-space/', 'Seven astronauts share their paths into the astronaut corps.', 'space history science'),
  ],
};
