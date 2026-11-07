#!/usr/bin/env perl
use strict;
use warnings;

my $found = 0;

sub rust_comment {
    my ($line, $state) = @_;
    while (length $line) {
        if ($$state ne 'code' && $$state ne 'string') {
            my $end = index($line, '"' . substr($$state, 4));
            return 0 if $end < 0;
            $line = substr($line, $end + 1 + length($$state) - 4);
            $$state = 'code';
        } elsif ($$state eq 'string') {
            if ($line =~ s/^\\.//s) { next; }
            if ($line =~ s/^"//) { $$state = 'code'; next; }
            $line = substr($line, 1);
        } elsif ($line =~ s/^r(\#*)"//) {
            $$state = 'raw:' . $1;
        } elsif ($line =~ s/^"//) {
            $$state = 'string';
        } elsif ($line =~ s/^'(?:\\.|[^'\\])'//) {
            next;
        } elsif ($line =~ m{^//|^/\*}) {
            return 1;
        } else {
            $line = substr($line, 1);
        }
    }
    return 0;
}

sub toml_comment {
    my ($line, $state) = @_;
    my $length = length $line;
    my $index = 0;

    while ($index < $length) {
        my $char = substr $line, $index, 1;
        my $triple = substr $line, $index, 3;

        if ($$state eq 'basic') {
            if ($char eq '\\') {
                $index++;
            } elsif ($char eq '"') {
                $$state = 'code';
            }
        } elsif ($$state eq 'literal') {
            $$state = 'code' if $char eq "'";
        } elsif ($$state eq 'multi_basic') {
            if ($char eq '\\') {
                $index++;
            } elsif ($triple eq '"""') {
                $$state = 'code';
                $index += 2;
            }
        } elsif ($$state eq 'multi_literal') {
            if ($triple eq "'''") {
                $$state = 'code';
                $index += 2;
            }
        } elsif ($triple eq '"""') {
            $$state = 'multi_basic';
            $index += 2;
        } elsif ($triple eq "'''") {
            $$state = 'multi_literal';
            $index += 2;
        } elsif ($char eq '"') {
            $$state = 'basic';
        } elsif ($char eq "'") {
            $$state = 'literal';
        } elsif ($char eq '#') {
            return 1;
        }

        $index++;
    }

    $$state = 'code' if $$state eq 'basic' || $$state eq 'literal';
    return 0;
}

for my $path (@ARGV) {
    open my $handle, '<', $path or die "cannot read $path: $!\n";
    my $is_toml = $path =~ /\.toml\z/;
    my $state = 'code';
    while (my $line = <$handle>) {
        my $comment = $is_toml ? toml_comment($line, \$state) : rust_comment($line, \$state);
        next unless $comment;
        print "$path:$.: $line";
        $found = 1;
    }
    close $handle;
}

exit $found;
