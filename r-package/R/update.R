#' Update uvr (R package + CLI binary)
#'
#' Convenience helper that updates both the \code{uvr} R companion package
#' (from GitHub) and the underlying \code{uvr} CLI binary in one call.
#' The two components are released together, and forgetting to update one
#' after the other is a common source of "works on my machine" mismatches.
#'
#' The function does \emph{not} automatically restart your R session. After
#' it completes, run \code{.rs.restartR()} (RStudio), \code{rstudioapi::restartSession()},
#' or simply restart R to load the newly installed package.
#'
#' @param ref Git ref (branch, tag, commit SHA) to install the R package from.
#'   Defaults to the default branch (\code{"HEAD"}).
#' @param method Binary install method. See \code{\link{install_uvr}} for
#'   the full set; defaults to \code{"auto"} (pre-built binary when available,
#'   cargo build otherwise).
#' @param quiet If \code{TRUE}, suppress progress messages.
#' @return Invisible named list with \code{r_package} (installed version or
#'   \code{NA} on failure) and \code{binary} (path to the CLI binary).
#' @export
#' @examples
#' \dontrun{
#' update_uvr()
#' # then restart R to pick up the new package
#' }
update_uvr <- function(ref = "HEAD", method = "auto", quiet = FALSE) {
  if (!quiet) message("Updating uvr R package from GitHub (ref: ", ref, ")...")
  repo_spec <- if (identical(ref, "HEAD") || is.null(ref)) {
    "nbafrank/uvr-r"
  } else {
    paste0("nbafrank/uvr-r@", ref)
  }

  # Prefer pak if available (faster, better diagnostics); fall back to remotes
  r_pkg_version <- NA_character_
  installed_ok <- FALSE
  if (requireNamespace("pak", quietly = TRUE)) {
    tryCatch({
      pak::pak(repo_spec, ask = FALSE, upgrade = TRUE)
      installed_ok <- TRUE
    }, error = function(e) {
      if (!quiet) message("pak install failed: ", conditionMessage(e))
    })
  }
  if (!installed_ok && requireNamespace("remotes", quietly = TRUE)) {
    tryCatch({
      remotes::install_github(repo_spec, quiet = quiet, upgrade = "never")
      installed_ok <- TRUE
    }, error = function(e) {
      if (!quiet) message("remotes install failed: ", conditionMessage(e))
    })
  }
  if (!installed_ok) {
    stop(
      "Could not update the uvr R package. Install 'pak' or 'remotes' first:\n",
      "  install.packages(\"pak\")    # or\n",
      "  install.packages(\"remotes\")",
      call. = FALSE
    )
  }
  # Query version without loading (DESCRIPTION already on disk post-install)
  r_pkg_version <- tryCatch(
    as.character(utils::packageVersion("uvr")),
    error = function(e) NA_character_
  )

  if (!quiet) message("Updating uvr CLI binary...")
  bin <- install_uvr(method = method, force = TRUE)

  if (!quiet) {
    message("")
    message("uvr update complete.")
    message("  R package : ", r_pkg_version)
    message("  CLI binary: ", bin)
    message("")
    message("Restart your R session to use the updated package:")
    if (nzchar(Sys.getenv("RSTUDIO"))) {
      message("  rstudioapi::restartSession()  # or Ctrl/Cmd+Shift+F10")
    } else {
      message("  q(\"no\"); R  # or restart R")
    }
  }

  invisible(list(r_package = r_pkg_version, binary = bin))
}
