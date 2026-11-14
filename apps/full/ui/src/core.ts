// The only bridge to the Rust core. Every call is a Tauri command implemented in src-tauri/src/main.rs.
import { invoke } from "@tauri-apps/api/core";

export interface LibraryCounts {
  songs: number;
  liked: number;
  playlists: number;
  artists: number;
  history_events: number;
}

export const core = {
  libraryCounts: () => invoke<LibraryCounts>("library_counts"),
};
