#' Find the uvr binary
#'
#' Searches for the \code{uvr} executable on the system PATH and common
#' installation locations. Stops with a helpful message if not found.
#'
#' @return The path to the \code{uvr} binary (character string).
#' @keywords internal
find_uvr <- function() {
  # Return cached result if available
  cached <- .uvr_env$bin
  if (!is.null(cached)) return(cached)

  path <- .find_uvr_path()
  if (!is.null(path)) {
    .uvr_env$bin <- path
    return(path)
  }

  if (interactive()) {
    message("The 'uvr' binary was not found on this system.")
    ans <- readline("Would you like to install it now? (Y/n) ")
    if (!nzchar(ans) || tolower(substr(ans, 1, 1)) == "y") {
      path <- install_uvr()
      return(path)
    }
  }

  stop(
    "Could not find the 'uvr' binary.\n",
    "Install it from R with: uvr::install_uvr()\n",
    "Or from the terminal: cargo install --git https://github.com/nbafrank/uvr",
    call. = FALSE
  )
}

#' Search common locations for the uvr binary
#' @return Path string or NULL if not found.
#' @keywords internal
.find_uvr_path <- function() {
  bin_name <- if (.Platform$OS.type == "windows") "uvr.exe" else "uvr"

  # Check PATH first
  path <- Sys.which(bin_name)
  if (nzchar(path)) return(unname(path))

  # Check common install locations
  if (.Platform$OS.type == "windows") {
    candidates <- file.path(Sys.getenv("USERPROFILE"), ".cargo", "bin", "uvr.exe")
    local_app <- Sys.getenv("LOCALAPPDATA")
    if (nzchar(local_app)) {
      candidates <- c(candidates, file.path(local_app, "Programs", "uvr", "uvr.exe"))
    }
  } else {
    candidates <- c(
      file.path(Sys.getenv("HOME"), ".cargo", "bin", "uvr"),
      "/usr/local/bin/uvr"
    )
  }
  for (candidate in candidates) {
    if (file.exists(candidate)) return(candidate)
  }

  NULL
}

# Package-level cache for binary path
.uvr_env <- new.env(parent = emptyenv())

#' Install the uvr binary
#'
#' Downloads and installs the \code{uvr} binary. Uses pre-built binaries from
#' GitHub releases when available, otherwise falls back to building from source
#' via \code{cargo install}.
#'
#' @param method Installation method: \code{"auto"} (default) tries GitHub
#'   release first then cargo, \code{"binary"} downloads a pre-built binary,
#'   \code{"cargo"} builds from source.
#' @param force If \code{TRUE}, reinstall even if already present.
#' @return Invisible path to the installed binary.
#' @export
install_uvr <- function(method = c("auto", "binary", "cargo"), force = FALSE) {
  method <- match.arg(method)

  if (!isTRUE(force)) {
    existing <- .find_uvr_path()
    if (!is.null(existing)) {
      message("uvr is already installed at: ", existing)
      message("Use install_uvr(force = TRUE) to reinstall.")
      return(invisible(existing))
    }
  }

  if (method == "auto" || method == "binary") {
    path <- .try_install_binary()
    if (!is.null(path)) {
      .uvr_env$bin <- path
      message("uvr installed successfully at: ", path)
      return(invisible(path))
    }
    if (method == "binary") {
      stop("No pre-built binary available for this platform.", call. = FALSE)
    }
  }

  # Fall back to cargo install
  .install_via_cargo()
}

#' Try to download a pre-built binary from GitHub releases
#' @return Path to binary or NULL if unavailable.
#' @keywords internal
.try_install_binary <- function() {
  if (!requireNamespace("jsonlite", quietly = TRUE)) {
    message("Package 'jsonlite' is needed to download pre-built binaries.")
    message("Install it with: install.packages('jsonlite')")
    return(NULL)
  }

  os <- .Platform$OS.type
  arch <- Sys.info()[["machine"]]

  # Map to GitHub release asset names
  target <- if (os == "unix" && Sys.info()[["sysname"]] == "Darwin") {
    if (arch %in% c("arm64", "aarch64")) "aarch64-apple-darwin" else "x86_64-apple-darwin"
  } else if (os == "unix") {
    if (arch %in% c("aarch64", "arm64")) "aarch64-unknown-linux-gnu" else "x86_64-unknown-linux-gnu"
  } else if (os == "windows") {
    # x86_64-pc-windows-msvc runs via emulation on Windows ARM64
    "x86_64-pc-windows-msvc"
  } else {
    return(NULL)
  }

  # Try latest release
  release_url <- "https://api.github.com/repos/nbafrank/uvr/releases/latest"
  con <- tryCatch(url(release_url), error = function(e) NULL)
  if (is.null(con)) return(NULL)
  on.exit(close(con), add = TRUE)

  resp <- tryCatch(
    jsonlite::fromJSON(readLines(con, warn = FALSE)),
    error = function(e) NULL
  )

  if (is.null(resp) || is.null(resp$assets)) return(NULL)

  # Find matching asset
  asset <- resp$assets[grepl(target, resp$assets$name, fixed = TRUE), ]
  if (nrow(asset) == 0L) return(NULL)

  download_url <- asset$browser_download_url[1L]
  if (.Platform$OS.type == "windows") {
    dest_dir <- file.path(Sys.getenv("USERPROFILE"), ".cargo", "bin")
  } else {
    dest_dir <- file.path(Sys.getenv("HOME"), ".cargo", "bin")
  }
  dir.create(dest_dir, recursive = TRUE, showWarnings = FALSE)
  bin_name <- if (.Platform$OS.type == "windows") "uvr.exe" else "uvr"
  dest <- file.path(dest_dir, bin_name)

  message("Downloading uvr from: ", download_url)
  tmp <- tempfile()
  ok <- tryCatch({
    utils::download.file(download_url, tmp, mode = "wb", quiet = TRUE)
    TRUE
  }, error = function(e) {
    message("Download failed: ", conditionMessage(e))
    FALSE
  })
  if (!ok) return(NULL)

  # Handle tar.gz, zip, or plain binary
  if (grepl("\\.tar\\.gz$", download_url)) {
    exdir <- tempfile("uvr-extract-")
    dir.create(exdir)
    on.exit(unlink(exdir, recursive = TRUE), add = TRUE)
    utils::untar(tmp, exdir = exdir)
    bin <- list.files(exdir, pattern = paste0("^", bin_name, "$"), recursive = TRUE, full.names = TRUE)[1L]
    if (is.na(bin)) return(NULL)
    file.copy(bin, dest, overwrite = TRUE)
  } else if (grepl("\\.zip$", download_url)) {
    exdir <- tempfile("uvr-extract-")
    dir.create(exdir)
    on.exit(unlink(exdir, recursive = TRUE), add = TRUE)
    utils::unzip(tmp, exdir = exdir)
    bin <- list.files(exdir, pattern = paste0("^", bin_name, "$"), recursive = TRUE, full.names = TRUE)[1L]
    if (is.na(bin)) return(NULL)
    file.copy(bin, dest, overwrite = TRUE)
  } else {
    file.copy(tmp, dest, overwrite = TRUE)
  }

  if (.Platform$OS.type != "windows") {
    Sys.chmod(dest, "0755")
  }
  dest
}

#' Install uvr via cargo
#' @return Invisible path to the installed binary.
#' @keywords internal
.install_via_cargo <- function() {
  cargo <- Sys.which("cargo")
  if (!nzchar(cargo)) {
    # Check common location
    home <- if (.Platform$OS.type == "windows") Sys.getenv("USERPROFILE") else Sys.getenv("HOME")
    cargo_candidate <- file.path(home, ".cargo", "bin", "cargo")
    if (file.exists(cargo_candidate)) {
      cargo <- cargo_candidate
    } else {
      stop(
        "Neither uvr binary nor cargo found.\n",
        "Install Rust first: https://rustup.rs\n",
        "Then run: uvr::install_uvr()",
        call. = FALSE
      )
    }
  }

  message("Building uvr from source via cargo (this may take a few minutes)...")
  rc <- system2(cargo, c("install", "--git", "https://github.com/nbafrank/uvr"),
                stdout = "", stderr = "")
  if (rc != 0L) {
    stop("cargo install failed with exit code ", rc, call. = FALSE)
  }

  home <- if (.Platform$OS.type == "windows") Sys.getenv("USERPROFILE") else Sys.getenv("HOME")
  bin_name <- if (.Platform$OS.type == "windows") "uvr.exe" else "uvr"
  path <- file.path(home, ".cargo", "bin", bin_name)
  if (!file.exists(path)) {
    stop("cargo install succeeded but uvr binary not found at expected location.", call. = FALSE)
  }

  .uvr_env$bin <- path
  message("uvr installed successfully at: ", path)
  invisible(path)
}

#' Run a uvr CLI command
#'
#' Internal helper that invokes uvr with the given arguments and streams
#' output to the R console.
#'
#' @param args Character vector of CLI arguments.
#' @param dir Optional working directory. Defaults to \code{getwd()}.
#' @param quiet If \code{TRUE}, suppress all output.
#' @return Invisible \code{TRUE} on success.
#' @keywords internal
run_uvr <- function(args, dir = NULL, quiet = FALSE) {
  bin <- find_uvr()

  if (!is.null(dir)) {
    old_wd <- setwd(dir)
    on.exit(setwd(old_wd), add = TRUE)
  }

  if (isTRUE(quiet)) {
    rc <- system2(bin, args, stdout = FALSE, stderr = FALSE)
  } else {
    rc <- system2(bin, args, stdout = "", stderr = "")
  }

  if (rc != 0L) {
    stop("uvr exited with code ", rc, call. = FALSE)
  }
  invisible(TRUE)
}
