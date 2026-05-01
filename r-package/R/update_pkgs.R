#' Update installed packages to the latest allowed versions
#'
#' Convenience wrapper that re-resolves the lockfile with
#' \code{lock(upgrade = TRUE)} and then installs the updates with
#' \code{sync()}. Equivalent to running \code{uvr lock --upgrade && uvr sync}
#' on the command line. (uvr-r issue #2.)
#'
#' Naming note: this is \code{update_pkgs()} rather than \code{update()} to
#' avoid masking \code{utils::update()}, which has a generic \code{update()}
#' for fitted models.
#'
#' @param dir Optional working directory. Defaults to \code{getwd()}.
#' @param quiet If \code{TRUE}, suppress output from both phases.
#' @return Invisible \code{TRUE} on success.
#' @export
#' @examples
#' \dontrun{
#' update_pkgs()
#' }
update_pkgs <- function(dir = NULL, quiet = FALSE) {
  lock(upgrade = TRUE, dir = dir, quiet = quiet)
  sync(dir = dir, quiet = quiet)
}
