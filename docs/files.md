# Files

## Search

Typing in the search field filters the current folder immediately. Enable
**Subfolders** to search local filenames recursively without blocking the user
interface. Results show their paths relative to the current folder, and a new
query or navigation cancels stale work.

Recursive search does not follow symbolic links and respects the hidden-files
toggle. Each search is limited to 500 results, 10,000 scanned folders, and 32
levels of depth. The status line identifies partial results when a limit is
reached or an inaccessible folder is skipped. Network locations and Trash keep
the current-folder filter because this first implementation is deliberately
local-only.

## File transfers

Local copy and move operations run on a worker so large files and folders do
not block the Files interface. The progress window reports the current path,
logical bytes completed, average throughput, and an estimated remaining time.
Cancellation is checked while each file is being copied; incomplete staged
output is removed, while top-level items completed earlier remain undoable.

When destination names conflict, Files asks whether to **Replace All**, **Skip
All**, or **Keep Both** before starting. New content is written to a private
sibling path and renamed into place only after it is complete. Replace keeps
the previous destination in a private backup for exact `Ctrl+Z` restoration;
the backup is removed when its undo record expires or the window closes.

Traversal is limited to 100,000 entries and 128 folder levels. Symbolic links
are copied as links rather than followed, hard links within a selected tree are
preserved, and regular-file/directory modes plus access and modification times
are retained. Special files are rejected during the preflight scan before any
changes are made. Remote GIO locations continue to use their native backend
transfer path.

## Trash

Files can restore selected Trash items to the original location. Restore never
overwrites an existing file or folder and leaves an item in Trash when its
original parent folder is unavailable. The Properties dialog shows the recorded
original location when the Trash backend provides it.

Selected Trash items can be removed with **Delete Permanently…** or
`Shift+Delete`. Both selected-item deletion and **Empty Trash…** require an
explicit confirmation and report partial failures; permanent deletion cannot be
undone.

## Archives

Select a local `.zip`, `.tar`, `.tar.gz`/`.tgz`, or `.tar.xz`/`.txz` archive
and use **Extract Here** or **Extract To…**. Extraction runs on a worker and can
be cancelled. Each archive gets its own destination folder; when that folder
already exists, **Keep Both** chooses a new name instead of overwriting it.

Files rejects absolute paths, parent traversal, symbolic and hard links,
devices, and other special entries. Extraction is limited to a 1 GiB archive,
10,000 entries, 512 MiB per file, 2 GiB total output, 64 path levels, and a
bounded compression ratio. Failed or cancelled jobs remove partial output.
Successful jobs can be undone with `Ctrl+Z`; undo removes only paths created by
the extraction and preserves anything added later. `.tar.xz` extraction uses
the system `xz` executable.

## Bulk rename

Select two or more local items in the same folder and press `F2` or choose
**Rename…**. The preview dialog supports find-and-replace, prefixes, suffixes,
sequential numbering, and upper- or lowercase conversion. File extensions are
preserved by default and can be included in the transformation when needed.

Files validates every generated name before changing anything. Empty or invalid
names, duplicate results, existing destinations, non-UTF-8 names, and filenames
over 255 bytes are rejected in the preview. Up to 500 items can be renamed at
once. Execution stages the complete batch under private temporary names, which
allows swaps and prevents partial renames; a failure rolls the batch back.
Successful batches are reversible with `Ctrl+Z` using the same transactional
path.

## Places bookmarks

Press `Ctrl+D`, use the bookmark button beside the path, or choose **Add to
Places** on a local folder to pin it in the sidebar. Bookmark context menus can
rename entries, move them up or down, open them in another tab or window, and
remove them without changing the folder itself. A folder that is temporarily
missing remains visible as unavailable so it can be repaired or removed.

Local Places entries use the standard GTK bookmark file at
`$XDG_CONFIG_HOME/gtk-3.0/bookmarks` (normally
`~/.config/gtk-3.0/bookmarks`), allowing compatible Linux file pickers to share
them. Files preserves unrelated and remote entries, bounds the file and entry
sizes, rejects a symlinked bookmark file, and updates it with an atomic private
write.

## Permissions

Open **Properties** for one or more local files or folders to inspect the owner,
group, symbolic mode, and numeric Unix mode. The permission grid edits read,
write, and execute access for the owner, group, and others. Mixed selections
retain any permission left in its mixed state, and regular files also provide
an **Allow executing as a program** shortcut.

Folder permissions can be applied to enclosed items after a separate
confirmation. Recursive changes are limited to 10,000 regular files and
folders, never follow symbolic links, and do not cross into mounted
filesystems. Files validates the complete batch first and restores already
changed modes if any update fails. A successful change records every previous
mode and can be restored exactly with `Ctrl+Z`. Ownership changes and
privileged permission elevation are deliberately not included.

## Network locations

Use **Connect to Server…** below the sidebar to open SMB, SFTP, FTP, FTPS,
WebDAV, or secure WebDAV locations. Files validates the address, rejects
embedded passwords, authentication parameters, query strings, and fragments,
then mounts it asynchronously through GIO. Login and certificate interaction
is owned by the platform mount dialog; FocalDesk never receives or writes the
password.

The optional **Remember this server** setting stores only the validated URI in
`$XDG_CONFIG_HOME/focaldesk/files-bookmarks` (normally
`~/.config/focaldesk/files-bookmarks`). The file is written atomically with
mode `0600`, its directory uses mode `0700`, and it is limited to 64 entries.
Bookmarks can reconnect a server and can be removed from their context menu.
Active remote mounts appear in Devices and use the same safe Unmount action as
other mounted volumes. Protocol availability depends on the installed GIO/GVfs
backends.

## Removable devices

Files discovers volumes and user-removable mounts through GIO and keeps the
Devices section of every sidebar current as hardware is connected, mounted,
unmounted, or removed. Selecting an unmounted volume opens the platform mount
and unlock dialog when required, then navigates to the mounted root. Mounted
devices provide Unmount or Eject in their context menu according to the
capabilities reported by the storage backend.

Mount, unmount, and eject operations are asynchronous and never invoke shell
commands. Files leaves a device before removing it, reports busy and
authentication errors without forcing removal, and discards hotplug signal
subscriptions when a tab is destroyed.

## Previews and thumbnails

Files generates thumbnails only for local regular files. JPEG and PNG decoding
runs on a worker with input-size, decoded-dimension, and allocation limits. PDF
preview uses `pdftoppm` for the first page only, scales to 256 pixels, suppresses
document output, and is terminated after five seconds. Unsupported, remote, or
oversized files retain their generic content-type icon.

Grid view schedules at most 64 thumbnails per render. Changing folders, search,
sort, or view generation cancels queued work and prevents stale results from
reaching the new view. The private cache lives below
`$XDG_CACHE_HOME/focaldesk/thumbnails/normal` (normally
`~/.cache/focaldesk/thumbnails/normal`) and is bounded to 512 entries and 128
MiB. Cached names use the conventional MD5-of-URI form and regenerate when the
source file is newer.

The selection pane shows image/PDF thumbnails, up to 64 KiB or 80 lines of
recognized local text, and bounded file metadata. Binary content is never
displayed as text, and selecting a file never executes it or fetches remote
content.

## Undo

Files keeps up to 20 completed file-operation records per tab. Rename, move,
copy, replacement, new-folder, drag-copy, and move-to-Trash operations show an
Undo action and can also be reversed with `Ctrl+Z`. Successful items from
partially failed or cancelled batches remain undoable. Failed or cancelled undo
steps stay in the history so they can be retried.

Undo is conflict-safe: it does not overwrite an item that has appeared at the
original path, and a newly created folder is removed only while it is still
empty. Emptying Trash and other permanent deletion are never added to the undo
history.
