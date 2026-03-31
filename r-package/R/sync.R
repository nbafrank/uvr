#' Synchronize project library from lockfile
#'
#' Installs all packages specified in \code{uvr.lock}. Idempotent --- skips
#' packages that are already installed. Equivalent to \code{uvr sync}.
#'
#' @param frozen If \code{TRUE}, fail if the lockfile is out of date (CI mode).
#' @param dir Optional working directory. Defaults to \code{getwd()}.
#' @param quiet If \code{TRUE}, suppress output.
#' @return Invisible \code{TRUE} on success.
#' @export
sync <- function(frozen = FALSE, dir = NULL, quiet = FALSE) {
  args <- "sync"
  if (isTRUE(frozen)) args <- c(args, "--frozen")
  run_uvr(args, dir = dir, quiet = quiet)
}
