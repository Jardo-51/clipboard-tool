# Clipbpard Tool Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Copying a file or directory in the file manager now records its path in the
  history, so it can be pasted as text. Previously such a copy carries no plain
  text — only a list of files — and was dropped on the floor. A multi-file copy
  becomes one entry with one path per line, and the rows are marked with a
  folder icon to tell them apart from the same string copied as text.
- `record_file_paths` config option (default `true`) to turn the above off, for
  anyone who would rather not have the files they copy in the file manager
  offered back as pastes. Turning it off leaves those copies out of the history
  entirely, rather than falling back to the text the file manager publishes
  alongside the files — that is the same copy by another name.

## [0.1.0] - 2026-07-29

First released version.

