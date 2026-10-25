#!/usr/bin/env perl
use strict;
use warnings;

my $found = 0;

for my $path (@ARGV) {
    open my $handle, '<', $path or die "cannot read $path: $!\n";
    while (my $line = <$handle>) {
        my $code = $line;
        $code =~ s/r(#*)".*?"\1//g;
        $code =~ s/"(?:\\.|[^"\\])*"//g;
        $code =~ s/'(?:\\.|[^'\\])'//g;
        next unless $code =~ m{//|/\*};
        print "$path:$.: $line";
        $found = 1;
    }
    close $handle;
}

exit $found;
