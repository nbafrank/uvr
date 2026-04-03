\name{run}
\alias{run}
\title{Run an R script in the project environment}
\usage{
run(script, args = character(), dir = NULL, quiet = FALSE)
}
\arguments{
\item{script}{Path to an R script.}
\item{args}{Character vector of arguments forwarded to the script.}
\item{dir}{Optional working directory. Defaults to \code{getwd()}.}
\item{quiet}{If \code{TRUE}, suppress output.}
}
\value{
Invisible \code{TRUE} on success.
}
\description{
Executes a script with the project library active.
Equivalent to \code{uvr run script.R} on the command line.
For interactive R, use the CLI directly: \code{uvr run}.
}
\examples{
\dontrun{
run("analysis.R")
run("model.R", args = c("--seed", "42"))
}
}
