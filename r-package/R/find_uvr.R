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

  # Check PATH first
  path <- Sys.which("uvr")
  if (nzchar(path)) {
    .uvr_env$bin <- unname(path)
    return(.uvr_env$bin)
  }

  # Check common install locations
  candidates <- c(
    file.path(Sys.getenv("HOME"), ".cargo", "bin", "uvr"),
    "/usr/local/bin/uvr"
  )
  for (candidate in candidates) {
    if (file.exists(candidate)) {
      .uvr_env$bin <- candidate
      return(candidate)
    }
  }

  stop(
    "Could not find the 'uvr' binary.\n",
    "Install it with:\n",
    "    cargo install --git https://github.com/nbafrank/uvr\n",
    "Or see https://github.com/nbafrank/uvr for other options.",
    call. = FALSE
  )
}

# Package-level cache for binary path
.uvr_env <- new.env(parent = emptyenv())

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
